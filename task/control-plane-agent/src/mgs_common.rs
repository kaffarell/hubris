// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{inventory::Inventory, update::sp::SpUpdate, Log, MgsMessage};
use core::convert::Infallible;
use drv_caboose::{CabooseError, CabooseReader};
use drv_sprot_api::{SpRot, SprotError, UpdateTarget};
use gateway_messages::{
    DiscoverResponse, ImageVersion, PowerState, ResetIntent, RotBootState,
    RotError, RotImageDetails, RotSlot, RotState, RotUpdateDetails,
    SpComponent, SpError, SpPort, SpState,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use static_assertions::const_assert;
use task_control_plane_agent_api::VpdIdentity;
use task_net_api::MacAddress;
use task_packrat_api::Packrat;
use userlib::{kipc, task_slot};

task_slot!(PACKRAT, packrat);

/// Provider of MGS handler logic common to all targets (gimlet, sidecar, psc).
pub(crate) struct MgsCommon {
    reset_requested: bool,
    reset_component_requested: SpComponent,
    inventory: Inventory,
    base_mac_address: MacAddress,
    packrat: Packrat,
}

impl MgsCommon {
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        Self {
            reset_requested: false,
            reset_component_requested: SpComponent {
                id: [0; SpComponent::MAX_ID_LENGTH],
            },
            inventory: Inventory::new(),
            base_mac_address,
            packrat: Packrat::from(PACKRAT.get_task_id()),
        }
    }

    pub(crate) fn packrat(&self) -> &Packrat {
        &self.packrat
    }

    pub(crate) fn discover(
        &mut self,
        port: SpPort,
    ) -> Result<DiscoverResponse, SpError> {
        ringbuf_entry!(Log::MgsMessage(MgsMessage::Discovery));
        Ok(DiscoverResponse { sp_port: port })
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        // We don't need to wait for packrat to be loaded: the sequencer task
        // for our board already does, and `net` waits for the sequencer before
        // starting. If we've gotten here, we've received a packet on the
        // network, which means `net` has started and the sequencer has already
        // populated packrat with what it read from our VPD.
        self.packrat.get_identity().unwrap_or_default()
    }

    pub(crate) fn sp_state(
        &mut self,
        update: &SpUpdate,
        power_state: PowerState,
    ) -> Result<SpState, SpError> {
        // SpState has extra-wide fields for the serial and model number. Below
        // when we fill them in we use `usize::min` to pick the right length
        // regardless of which is longer, but really we want to know we aren't
        // truncating our values. We'll statically assert that `SpState`'s field
        // length is wider than `VpdIdentity`'s to catch this early.
        const SP_STATE_FIELD_WIDTH: usize = 32;
        const_assert!(SP_STATE_FIELD_WIDTH >= VpdIdentity::SERIAL_LEN);
        const_assert!(SP_STATE_FIELD_WIDTH >= VpdIdentity::PART_NUMBER_LEN);

        ringbuf_entry!(Log::MgsMessage(MgsMessage::SpState));

        let id = self.identity();

        let mut state = SpState {
            serial_number: [0; SP_STATE_FIELD_WIDTH],
            model: [0; SP_STATE_FIELD_WIDTH],
            revision: id.revision,
            hubris_archive_id: kipc::read_image_id().to_le_bytes(),
            base_mac_address: self.base_mac_address.0,
            version: update.current_version(),
            power_state,
            rot: rot_state(update.sprot_task()),
        };

        let n = usize::min(state.serial_number.len(), id.serial.len());
        state.serial_number[..n].copy_from_slice(&id.serial);

        let n = usize::min(state.model.len(), id.part_number.len());
        state.model[..n].copy_from_slice(&id.part_number);

        Ok(state)
    }

    pub(crate) fn reset_prepare(&mut self) -> Result<(), SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ResetPrepare));
        self.reset_requested = true;
        Ok(())
    }

    pub(crate) fn reset_trigger(&mut self) -> Result<Infallible, SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        if !self.reset_requested {
            return Err(SpError::ResetTriggerWithoutPrepare);
        }

        let jefe = task_jefe_api::Jefe::from(crate::JEFE.get_task_id());
        jefe.request_reset();

        // If `request_reset()` returns, something has gone very wrong.
        panic!()
    }

    #[inline(always)]
    pub(crate) fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    pub(crate) fn get_caboose_value(
        &self,
        key: [u8; 4],
    ) -> Result<&'static [u8], SpError> {
        let reader = userlib::kipc::get_caboose()
            .map(CabooseReader::new)
            .ok_or(SpError::NoCaboose)?;
        reader.get(key).map_err(|e| match e {
            CabooseError::NoSuchTag => SpError::NoSuchCabooseKey(key),
            CabooseError::MissingCaboose => SpError::NoCaboose,
            CabooseError::TlvcReaderBeginFailed => SpError::CabooseReadError,
            CabooseError::TlvcReadExactFailed => SpError::CabooseReadError,
            CabooseError::BadChecksum => SpError::BadCabooseChecksum,

            // NoImageHeader is only returned when reading the caboose of the
            // bank2 slot; it shouldn't ever be returned by the local reader.
            CabooseError::NoImageHeader => panic!(),
        })
    }

    pub(crate) fn reset_component_prepare(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        ringbuf_entry!(Log::MgsMessage(MgsMessage::ResetPrepare));
        self.reset_component_requested = component;
        Ok(())
    }

    pub(crate) fn reset_component_trigger(
        &mut self,
        update: &SpUpdate,
        component: SpComponent,
        slot: Option<u16>,
        intent: ResetIntent,
        auth_data: &[u8],
    ) -> Result<(), SpError> {
        // TODO: Add some kind of auth check before performing a reset.
        // https://github.com/oxidecomputer/hubris/issues/723
        if self.reset_component_requested != component {
            return Err(SpError::ResetComponentTriggerWithoutPrepare);
        }
        // If we are not resetting the SP_ITSELF, then we may come back here.
        self.reset_component_requested.id.fill(0);

        // For now, resetting the SP through reset_component() is
        // the same as through reset()
        if component == SpComponent::SP_ITSELF {
            task_jefe_api::Jefe::from(crate::JEFE.get_task_id())
                .request_reset();
            // If `request_reset()` returns,
            // something has gone very wrong.
            panic!();
        }

        // mgs_{gimlet,psc,sidecar}.rs deal with any board specific
        // reset strategy. Here we take care of common SP and RoT cases.
        let target = if matches!(
            intent,
            ResetIntent::Persistent | ResetIntent::Transient
        ) {
            match component {
                SpComponent::ROT => match slot {
                    Some(0) => UpdateTarget::ImageA,
                    Some(1) => UpdateTarget::ImageB,
                    _ => return Err(SpError::RequestUnsupportedForComponent),
                },
                _ => return Err(SpError::RequestUnsupportedForComponent),
            }
        } else {
            // TODO: This value is ignored when intent is Normal or Expensive*
            UpdateTarget::ImageA
        };
        match update.sprot_task().reset_component(
            intent.into(),
            target,
            auth_data.len() as u16,
            auth_data,
        ) {
            Err(SprotError::RspTimeout) => {
                // This is the expected error if the reset was successful.
                // It could be that the RoT is out-to-lunch for some other
                // reason though.
                // Things for upper layers to do:
                // TODO: Check boot nonce to see if we are in a new session.
                // TODO: Check that the expected image is now running.
                // (Management plane should do that.)
                // TODO: Enable staged updates where we don't automatically
                // reset after writing an image.
                ringbuf_entry!(Log::RotReset {
                    err: SprotError::RspTimeout
                });
            }
            Err(err) => {
                // Some other error occurred.
                // TODO: Update is all-or-nothing at the moment.
                // The control plane can try to reset the RoT again or it
                // can start the update process all over again.  We should
                // be able to make incremental progress if there is some
                // bug/condition that is degrading SpRot communications.
                ringbuf_entry!(Log::RotReset { err });
                // TODO: send an error back. However, changes need to be
                // made to the management plane for it to understand that
                // the write was successful but the new image is not yet
                // running.
            }
            _ => {
                // We cannot get here given the RoT's current
                // implementation. It will either
                // reset, forgetting about our request,
                // or reject our request with an error.
                // TODO: Similar to RoT's transient boot selection,
                // we could leave a reminder to the next RoT session
                // to send a response to the reset request.
                panic!()
            }
        }
        Ok(())
    }
}

// conversion between gateway_messages types and hubris types is quite tedious.
fn rot_state(sprot: &SpRot) -> Result<RotState, RotError> {
    let boot_state = sprot.status().map_err(SprotErrorConvert)?.rot_updates;
    let active = match boot_state.active {
        drv_update_api::RotSlot::A => RotSlot::A,
        drv_update_api::RotSlot::B => RotSlot::B,
    };

    let slot_a = boot_state.a.map(|a| RotImageDetailsConvert(a).into());
    let slot_b = boot_state.b.map(|b| RotImageDetailsConvert(b).into());

    Ok(RotState {
        rot_updates: RotUpdateDetails {
            boot_state: RotBootState {
                active,
                slot_a,
                slot_b,
            },
        },
    })
}

pub(crate) struct SprotErrorConvert(pub drv_sprot_api::SprotError);

impl From<SprotErrorConvert> for RotError {
    fn from(err: SprotErrorConvert) -> Self {
        RotError::MessageError { code: err.0 as u32 }
    }
}

pub(crate) struct RotImageDetailsConvert(pub drv_update_api::RotImageDetails);

impl From<RotImageDetailsConvert> for RotImageDetails {
    fn from(value: RotImageDetailsConvert) -> Self {
        RotImageDetails {
            digest: value.0.digest,
            version: ImageVersion {
                epoch: value.0.version.epoch,
                version: value.0.version.version,
            },
        }
    }
}
