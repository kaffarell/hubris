// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dump Agent

#![no_std]
#![no_main]

use core::mem::size_of;
use dump_agent_api::*;
use idol_runtime::RequestError;
use static_assertions::const_assert;
use task_jefe_api::Jefe;
use userlib::*;

//
// Our DUMP_READ_SIZE must be an even power of 2 -- and practically speaking
// cannot be more than 1K
//
const_assert!(DUMP_READ_SIZE & (DUMP_READ_SIZE - 1) == 0);
const_assert!(DUMP_READ_SIZE <= 1024);

struct ServerImpl {
    areas: [DumpArea; 3],
}

#[cfg(not(feature = "no-rot"))]
task_slot!(SPROT, sprot);

task_slot!(JEFE, jefe);

impl ServerImpl {
    fn initialize(&self) {
        let jefe = Jefe::from(JEFE.get_task_id());
        jefe.initialize_dump_areas();
    }

/*
    fn get_area(
        &self,
        index: u8,
        readonly: bool
    ) -> Result<DumpArea, DumpAgentError> {
        let jefe = Jefe::from(JEFE.get_task_id());
        match jefe.get_dump_area_header(index, readonly) {
            Ok(header) => Ok(DumpHeader {
                address: header.
        
    }
*/

    fn add_dump_segment(
        &mut self,
        addr: u32,
        length: u32,
    ) -> Result<(), DumpAgentError> {
        let area = self.areas[0];
        
        unsafe {
            let header = area.address as *mut DumpAreaHeader;

            if (*header).magic != DUMP_MAGIC {
                panic!("bad dump magic!");
            }

            let nsegments = (*header).nsegments;

            let offset = size_of::<DumpAreaHeader>()
                + (nsegments as usize) * size_of::<DumpSegmentHeader>();
            let need = (offset + size_of::<DumpSegmentHeader>()) as u32;

            if need > (*header).length {
                return Err(DumpAgentError::OutOfSpaceForSegments);
            }

            let saddr = area.address as usize + offset;
            let segment = saddr as *mut DumpSegmentHeader;

            (*segment).address = addr;
            (*segment).length = length;

            (*header).nsegments = nsegments + 1;
            (*header).written = need;
        }

        Ok(())
    }
}

impl idl::InOrderDumpAgentImpl for ServerImpl {
    fn get_dump_area(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
    ) -> Result<DumpArea, RequestError<DumpAgentError>> {
//        match self.get_area(index, false


        if index as usize >= self.areas.len() {
            Err(DumpAgentError::InvalidArea.into())
        } else {
            Ok(self.areas[index as usize])
        }
    }

    fn initialize_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        self.initialize();
        Ok(())
    }

    fn add_dump_segment(
        &mut self,
        _msg: &RecvMessage,
        address: u32,
        length: u32,
    ) -> Result<(), RequestError<DumpAgentError>> {
        if address & 0b11 != 0 {
            return Err(DumpAgentError::UnalignedSegmentAddress.into());
        }

        if (length as usize) & 0b11 != 0 {
            return Err(DumpAgentError::UnalignedSegmentLength.into());
        }

        self.add_dump_segment(address, length)?;

        Ok(())
    }

    #[cfg(not(feature = "no-rot"))]
    fn take_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        let sprot = drv_sprot_api::SpRot::from(SPROT.get_task_id());
        let mut buf = [0u8; 4];

        match sprot.send_recv(
            drv_sprot_api::MsgType::DumpReq,
            &self.areas[0].address.to_le_bytes(),
            &mut buf,
        ) {
            Err(_) => Err(DumpAgentError::DumpFailed.into()),
            Ok(_) => Ok(()),
        }
    }

    #[cfg(feature = "no-rot")]
    fn take_dump(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<DumpAgentError>> {
        Err(DumpAgentError::NotSupported.into())
    }

    //
    // We return a buffer of fixed size here instead of taking a lease
    // because we want/need this to work with consumers who are not
    // lease aware (specifically, udprpc and hiffy).
    //
    fn read_dump(
        &mut self,
        _msg: &RecvMessage,
        index: u8,
        offset: u32,
    ) -> Result<[u8; DUMP_READ_SIZE], RequestError<DumpAgentError>> {
        let mut rval = [0u8; DUMP_READ_SIZE];

        if offset & ((rval.len() as u32) - 1) != 0 {
            return Err(DumpAgentError::UnalignedOffset.into());
        }

        // let area = self.get_area(index, Area::ReadOnly);

        let area = self
            .areas
            .get(index as usize)
            .ok_or(DumpAgentError::InvalidArea)?;

        let written = unsafe {
            let header = area.address as *mut DumpAreaHeader;
            core::ptr::read_volatile(header).written
        };

        if written > offset {
            let to_read = written - offset;
            let base = area.address as *const u8;
            let base = unsafe { base.add(offset as usize) };

            for i in 0..usize::min(to_read as usize, DUMP_READ_SIZE) {
                rval[i] = unsafe { core::ptr::read_volatile(base.add(i)) };
            }

            Ok(rval)
        } else {
            Err(DumpAgentError::BadOffset.into())
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {
        areas: [
            DumpArea {
                address: 0x30020000,
                length: 0x20000,
            },
            DumpArea {
                address: 0x30040000,
                length: 0x8000,
            },
            DumpArea {
                address: 0x38000000,
                length: 0x10000,
            },
        ],
    };

    server.initialize();

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::*;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
