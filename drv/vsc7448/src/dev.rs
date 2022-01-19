// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    port::{port10g_flush, port1g_flush},
    serdes10g,
    spi::Vsc7448Spi,
    VscError,
};
use vsc7448_pac::Vsc7448;

/// DEV1G and DEV2G5 share the same register layout, so we can write functions
/// that use either one.
#[derive(Copy, Clone)]
pub enum DevGeneric {
    Dev1g(u32),
    Dev2g5(u32),
}

impl DevGeneric {
    pub fn new_1g(d: u32) -> Self {
        assert!(d < 24);
        DevGeneric::Dev1g(d)
    }
    pub fn new_2g5(d: u32) -> Self {
        assert!(d < 29);
        DevGeneric::Dev2g5(d)
    }
    pub fn port(&self) -> u32 {
        match *self {
            DevGeneric::Dev1g(d) => {
                if d < 8 {
                    d
                } else {
                    d + 24
                }
            }
            DevGeneric::Dev2g5(d) => {
                if d <= 24 {
                    d + 8
                } else {
                    d + 24
                }
            }
        }
    }
    pub fn regs(&self) -> vsc7448_pac::DEV1G {
        match *self {
            DevGeneric::Dev1g(d) => Vsc7448::DEV1G(d),
            DevGeneric::Dev2g5(d) =>
            // We know that d is < 29 based on the check in the constructor.
            // DEV1G and DEV2G5 register blocks are identical in layout and
            // tightly packed, and there are 28 DEV2G5 register blocks, so
            // this should be a safe trick.
            {
                vsc7448_pac::DEV1G::from_raw_unchecked_address(
                    vsc7448_pac::DEV2G5::BASE + d * vsc7448_pac::DEV1G::SIZE,
                )
            }
        }
    }
    pub fn index(&self) -> u32 {
        match *self {
            DevGeneric::Dev1g(d) | DevGeneric::Dev2g5(d) => d,
        }
    }
}

/// Wrapper struct for a DEV10G index, which is analogous to `DevGeneric`.
/// The DEV10G target registers aren't identical to the DEV1G, so we need
/// to handle it differently.
#[derive(Copy, Clone)]
pub struct Dev10g(u32);
impl Dev10g {
    pub fn new(d: u32) -> Self {
        assert!(d < 4);
        Self(d)
    }
    /// Converts from a DEV10G index to a port index
    pub fn port(&self) -> u32 {
        self.0 + 49
    }
    pub fn regs(&self) -> vsc7448_pac::DEV10G {
        Vsc7448::DEV10G(self.0)
    }
    pub fn index(&self) -> u32 {
        self.0
    }
}

/// Based on `jr2_port_conf_1g_set` in the SDK
pub fn dev1g_init_sgmii(
    dev: DevGeneric,
    v: &Vsc7448Spi,
) -> Result<(), VscError> {
    // Flush the port before doing anything else
    port1g_flush(dev, v)?;

    // Enable full duplex mode and GIGA SPEED
    let dev1g = dev.regs();
    v.modify(dev1g.MAC_CFG_STATUS().MAC_MODE_CFG(), |r| {
        r.set_fdx_ena(1);
        r.set_giga_mode_ena(1);
    })?;

    v.modify(dev1g.MAC_CFG_STATUS().MAC_IFG_CFG(), |r| {
        // NOTE: these are speed-dependent options and aren't
        // fully documented in the manual; this values are chosen
        // based on the SDK for 1G, full duplex operation.
        r.set_tx_ifg(4);
        r.set_rx_ifg1(0);
        r.set_rx_ifg2(0);
    })?;

    // The upcoming steps depend on how the port is talking to the
    // outside world (100FX / SGMII / SERDES).  In this case, the port
    // is talking over QSGMII, which is configured like SGMII then
    // combined in the macro block (I may be butchering some details of
    // terminology or architecture here).

    // The device is configured to SGMII mode by default, so no
    // changes are needed there.

    // This bit isn't documented in the datasheet, but the SDK says it
    // must be set in SGMII mode.  It allows a link to be set up by
    // software, even if autonegotiation fails.
    v.modify(dev1g.PCS1G_CFG_STATUS().PCS1G_ANEG_CFG(), |r| {
        r.set_sw_resolve_ena(1)
    })?;

    // Configure signal detect line with values from the dev kit
    // This is dependent on the port setup.
    v.modify(dev1g.PCS1G_CFG_STATUS().PCS1G_SD_CFG(), |r| {
        r.set_sd_ena(0); // Ignored
    })?;

    // Enable the PCS!
    v.modify(dev1g.PCS1G_CFG_STATUS().PCS1G_CFG(), |r| r.set_pcs_ena(1))?;

    // The SDK configures MAC VLAN awareness here; let's not do that
    // for the time being.

    // The SDK also configures flow control (`jr2_port_fc_setup`)
    // and policer flow control (`vtss_jr2_port_policer_fc_set`) around
    // here, which we'll skip.

    // Turn on the MAC!
    v.write_with(dev1g.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| {
        r.set_tx_ena(1);
        r.set_rx_ena(1);
    })?;

    // Take MAC, Port, Phy (intern), and PCS (SGMII) clocks out of
    // reset, turning on a 1G port data rate.
    v.write_with(dev1g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
        r.set_speed_sel(2)
    })?;

    let port = dev.port();
    v.modify(Vsc7448::QFWD().SYSTEM().SWITCH_PORT_MODE(port), |r| {
        r.set_port_ena(1);
        r.set_fwd_urgency(104); // This is different based on speed
    })?;

    Ok(())
}

pub fn dev10g_init_sfi(
    dev: Dev10g,
    serdes_cfg: &serdes10g::Config,
    v: &Vsc7448Spi,
) -> Result<(), VscError> {
    // jr2_sd10g_xfi_mode
    v.modify(Vsc7448::XGXFI(dev.index()).XFI_CONTROL().XFI_MODE(), |r| {
        r.set_sw_rst(0);
        r.set_endian(1);
        r.set_sw_ena(1);
    })?;

    // jr2_sd10g_cfg, moved into a separate function because bringing
    // up a 10G SERDES is _hard_
    let serdes_index = dev.index();
    serdes_cfg.apply(serdes_index, v)?;
    port10g_flush(dev, v)?;

    // Remaining logic is from `jr2_port_conf_10g_set`
    // Handle signal detect
    let dev10g = dev.regs();
    v.modify(dev10g.PCS_XAUI_CONFIGURATION().PCS_XAUI_SD_CFG(), |r| {
        r.set_sd_ena(0);
    })?;
    // Enable SFI PCS
    v.modify(dev10g.PCS_XAUI_CONFIGURATION().PCS_XAUI_CFG(), |r| {
        r.set_pcs_ena(1);
    })?;
    v.modify(dev10g.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| {
        r.set_rx_ena(1);
        r.set_tx_ena(1);
    })?;
    v.modify(dev10g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
        r.set_pcs_rx_rst(0);
        r.set_pcs_tx_rst(0);
        r.set_mac_rx_rst(0);
        r.set_mac_tx_rst(0);
        r.set_speed_sel(7); // SFI
    })?;
    v.modify(Vsc7448::QFWD().SYSTEM().SWITCH_PORT_MODE(dev.port()), |r| {
        r.set_port_ena(1);
        r.set_fwd_urgency(9);
    })?;

    Ok(())
}
