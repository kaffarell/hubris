// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use drv_caboose::CabooseError;
pub use drv_update_api::*;
use userlib::sys_send;

// This value is currently set to `lpc55_romapi::FLASH_PAGE_SIZE`
//
// We hardcode it for simplicity, and because we cannot,and should not,
// directly include the `lpc55_romapi` crate. While we could transfer
// arbitrary amounts of data over spi and have the update server on
// the RoT split it up, this makes the code more complicated than
// necessary and is only an optimization. For now, especially since we
// only have 1 RoT and we must currently define a constant for use the
// `control_plane_agent::ComponentUpdater` trait, we do the simple thing
// and hardcode according to hardware requirements.
pub const BLOCK_SIZE_BYTES: usize = 512;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
