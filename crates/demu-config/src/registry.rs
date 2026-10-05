//! The kind registry: the contract between the configuration and the
//! compiled device-family crates.
//!
//! A device-family crate registers a [`KindInfo`] under its stable kind
//! key, for example `"sm83.cpu"`. The registry validates that a
//! configuration selects known kinds and that each kind declares the
//! domain behavior and quirk names the config may use.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;

use crate::newtypes::KindName;

/// How a device kind relates to clock domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainRequirement {
    /// The device must declare a domain.
    Required,
    /// The device declares no domain.
    None,
}

/// The declared capabilities of one device kind.
///
/// Quirk names are bare keys scoped to the kind. A config spells the
/// quirk `<family>.<quirk>`, and the validation checks the family part
/// against the kinds the config selects.
#[derive(Debug, Clone)]
pub struct KindInfo {
    /// The stable kind key.
    pub kind: KindName,
    /// The domain requirement of the kind.
    pub domain: DomainRequirement,
    /// The quirk names the kind accepts, scoped to this kind.
    pub quirks: &'static [&'static str],
}

impl KindInfo {
    /// Declare a kind.
    ///
    /// # Panics
    ///
    /// Panics when the kind key does not match the `<family>.<kind>`
    /// grammar. A registration site compiles its own key, so a bad key is
    /// a programmer error, not a runtime condition.
    pub fn new(
        kind: &'static str,
        domain: DomainRequirement,
        quirks: &'static [&'static str],
    ) -> Self {
        match KindName::new(kind) {
            Ok(kind) => Self {
                kind,
                domain,
                quirks,
            },
            Err(_) => panic!("invalid kind key: {kind}"),
        }
    }
}

/// The registry that maps kind keys to their declared info.
#[derive(Debug, Default)]
pub struct KindRegistry {
    kinds: FxHashMap<KindName, KindInfo>,
}

impl KindRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one kind. Registration order is the tie-break order for
    /// validation messages.
    pub fn register(&mut self, info: KindInfo) {
        self.kinds.insert(info.kind.clone(), info);
    }

    /// Look up one kind.
    pub fn get(&self, kind: &KindName) -> Option<&KindInfo> {
        self.kinds.get(kind)
    }

    /// All registered kinds, ordered by kind key.
    pub fn kinds(&self) -> Vec<&KindInfo> {
        let mut v: Vec<&KindInfo> = self.kinds.values().collect();
        v.sort_by(|a, b| a.kind.cmp(&b.kind));
        v
    }
}

/// The process-wide registry used by bundled configs and validation.
///
/// The core registry holds the synthetic `smoke.*` kinds that the smoke
/// config and framework tests use. Quirk names are bare keys scoped to the
/// kind; the config spells them as `<family>.<quirk>`. Device-family
/// crates add their kinds here as they land.
pub fn core_registry() -> &'static KindRegistry {
    static REGISTRY: OnceLock<KindRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut r = KindRegistry::new();
        r.register(KindInfo::new(
            "smoke.cpu",
            DomainRequirement::Required,
            &["trace"],
        ));
        r.register(KindInfo::new("smoke.ram", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("smoke.rom", DomainRequirement::Required, &[]));
        // The SM83 machine family: Game Boy and Game Boy Color.
        r.register(KindInfo::new(
            "sm83.cpu",
            DomainRequirement::Required,
            &["oam_bug", "wave_ram_undoc", "halt_undoc"],
        ));
        for kind in [
            "sm83.wram",
            "sm83.hram",
            "sm83.bootrom",
            "sm83.cart",
            "sm83.dma",
            "sm83.hdma",
            "sm83.timer",
            "sm83.joypad",
            "sm83.serial",
            "sm83.io",
        ] {
            r.register(KindInfo::new(kind, DomainRequirement::Required, &[]));
        }
        r.register(KindInfo::new("sm83.ppu", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("sm83.apu", DomainRequirement::Required, &[]));
        // The NES device family of the RP2A03 (NTSC) and RP2A07
        // (PAL). The kind names stay static at the registration
        // sites; both chip families parameterize one device set.
        r.register(KindInfo::new(
            "rp2A03.cpu",
            DomainRequirement::Required,
            &["undocumented"],
        ));
        r.register(KindInfo::new(
            "rp2A07.cpu",
            DomainRequirement::Required,
            &["undocumented"],
        ));
        r.register(KindInfo::new(
            "mos6502.cpu",
            DomainRequirement::Required,
            &["undocumented"],
        ));
        for (prefix, kind) in [
            ("rp2A03", "wram"),
            ("rp2A07", "wram"),
            ("mos6502", "wram"),
            ("rp2A03", "cart"),
            ("rp2A07", "cart"),
            ("mos6502", "cart"),
            ("rp2A03", "dma"),
            ("rp2A07", "dma"),
            ("mos6502", "dma"),
            ("rp2A03", "joypad"),
            ("rp2A07", "joypad"),
            ("mos6502", "joypad"),
            ("rp2A03", "ppu"),
            ("rp2A07", "ppu"),
            ("mos6502", "ppu"),
            ("rp2A03", "apu"),
            ("rp2A07", "apu"),
            ("mos6502", "apu"),
        ] {
            let key: &'static str = match (prefix, kind) {
                ("rp2A03", "wram") => "rp2A03.wram",
                ("rp2A07", "wram") => "rp2A07.wram",
                ("rp2A03", "cart") => "rp2A03.cart",
                ("rp2A07", "cart") => "rp2A07.cart",
                ("rp2A03", "dma") => "rp2A03.dma",
                ("rp2A07", "dma") => "rp2A07.dma",
                ("rp2A03", "joypad") => "rp2A03.joypad",
                ("rp2A07", "joypad") => "rp2A07.joypad",
                ("rp2A03", "ppu") => "rp2A03.ppu",
                ("rp2A07", "ppu") => "rp2A07.ppu",
                ("rp2A03", "apu") => "rp2A03.apu",
                ("rp2A07", "apu") => "rp2A07.apu",
                ("mos6502", "wram") => "mos6502.wram",
                ("mos6502", "cart") => "mos6502.cart",
                ("mos6502", "dma") => "mos6502.dma",
                ("mos6502", "joypad") => "mos6502.joypad",
                ("mos6502", "ppu") => "mos6502.ppu",
                _ => "mos6502.apu",
            };
            r.register(KindInfo::new(key, DomainRequirement::Required, &[]));
        }
        // The Atari machine family: the 2600 and the video slice and
        // sound chips of the 5200 and the 7800. The CPU kinds reuse
        // the plain 6502 kind with divider overrides from the config.
        r.register(KindInfo::new("atari.pia", DomainRequirement::Required, &[]));
        // The Commodore 64 of the 6502 family.
        r.register(KindInfo::new("c64.vic", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("c64.sid", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("c64.cia", DomainRequirement::Required, &[]));
        // The Apple II family.
        r.register(KindInfo::new("a2.video", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("a2.io", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("a2.disk2", DomainRequirement::Required, &[]));
        for kind in [
            "atari.tia",
            "atari.riot",
            "atari.antic",
            "atari.pokey",
            "atari.maria",
            "atari.ram",
            "atari.rom",
            "atari.cart2600",
            "atari.cart5200",
            "atari.cart7800",
        ] {
            r.register(KindInfo::new(kind, DomainRequirement::Required, &[]));
        }
        // The 65C816 machine families: the SNES and the Apple IIgs.
        // The SNES audio die carries the port-race quirk; the SPC
        // and DSP form one device on the sound domain.
        r.register(KindInfo::new(
            "wdc65c816.cpu",
            DomainRequirement::Required,
            &[],
        ));
        for kind in [
            "snes.ppu",
            "snes.sys",
            "snes.wram",
            "snes.pad",
            "snes.cart",
            "iigs.doc",
            "iigs.mega2",
            "iigs.iwm",
        ] {
            r.register(KindInfo::new(kind, DomainRequirement::Required, &[]));
        }
        r.register(KindInfo::new(
            "apu.core",
            DomainRequirement::Required,
            &["port_race_or"],
        ));
        // The 65SC02 machine families: the Lynx and the plain CMOS
        // core. Each kind gate carries the page-bit flavor quirk.
        r.register(KindInfo::new(
            "vl65nc02.cpu",
            DomainRequirement::Required,
            &["bbr_smb"],
        ));
        r.register(KindInfo::new(
            "mos65sc02.cpu",
            DomainRequirement::Required,
            &["bbr_smb"],
        ));
        // The HuC6280 die of the PC Engine: the MMU, the timer, and
        // the interrupt controller sit on the chip.
        r.register(KindInfo::new(
            "huc6280.cpu",
            DomainRequirement::Required,
            &[],
        ));
        // The Lynx device family around the VTI die: the Suzy sprite
        // die with the PWM sound, the Mikey die, the map control, the
        // memory slices, and the stream cartridge.
        r.register(KindInfo::new("lynx.suzy", DomainRequirement::Required, &[]));
        r.register(KindInfo::new(
            "lynx.mikey",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "lynx.mapctl",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new("lynx.ram", DomainRequirement::Required, &[]));
        r.register(KindInfo::new(
            "lynx.bootrom",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new("lynx.cart", DomainRequirement::Required, &[]));
        // The PC Engine family: the VDC and VCE video pair, the PSG,
        // the controller port, the flat work RAM, and the HuCard.
        r.register(KindInfo::new("pce.vdc", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("pce.vce", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("pce.psg", DomainRequirement::Required, &[]));
        r.register(KindInfo::new(
            "pce.joypad",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new("pce.ram", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("pce.cart", DomainRequirement::Required, &[]));
        // The Z80 machine family: the NMOS core, the plain memory
        // pieces, the banked ROM of the banked Sinclair models, the
        // VDP family of the Sega systems, the SG76489-class PSG, the
        // Sega I/O controller, the Sinclair ULA, and the TI-85
        // memory, display, and link pieces.
        r.register(KindInfo::new(
            "z80.cpu",
            DomainRequirement::Required,
            &["undocumented"],
        ));
        r.register(KindInfo::new("z80.ram", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("z80.rom", DomainRequirement::Required, &[]));
        r.register(KindInfo::new(
            "z80.bankrom",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "zx.bankram",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "tms9918.vdp",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new("sms.vdp", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("gg.vdp", DomainRequirement::Required, &[]));
        r.register(KindInfo::new(
            "sn76489.psg",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "sega.io",
            DomainRequirement::Required,
            &["gg_start"],
        ));
        r.register(KindInfo::new("sega.cart", DomainRequirement::Required, &[]));
        r.register(KindInfo::new(
            "zx.ula",
            DomainRequirement::Required,
            &["zx81_slow"],
        ));
        r.register(KindInfo::new("ti85.mem", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("ti85.lcd", DomainRequirement::Required, &[]));
        r.register(KindInfo::new("ti85.link", DomainRequirement::Required, &[]));
        // The 68000 family: the core, the Genesis machines (the VDP,
        // the support silicon, and the cart), and the YM sound chips.
        r.register(KindInfo::new(
            "m68000.cpu",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "genesis.vdp",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "genesis.arbiter",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "genesis.io",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "genesis.mapper",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "genesis.cart",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "ym2612.psg",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "ym2610.chip",
            DomainRequirement::Required,
            &[],
        ));
        // The Neo Geo family: the video die, the palette chip, the
        // cart's P chip, the two IO dies, the M1 sound program ROM,
        // and the generic read-only form for the S/C/V image chips
        // the video and sound dies read through their parameters.
        r.register(KindInfo::new(
            "neogeo.lspc",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "neogeo.palette",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "neogeo.cart",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new("neogeo.io", DomainRequirement::Required, &[]));
        r.register(KindInfo::new(
            "neogeo.soundio",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "neogeo.m1rom",
            DomainRequirement::Required,
            &[],
        ));
        r.register(KindInfo::new(
            "neogeo.rom",
            DomainRequirement::Required,
            &[],
        ));
        r
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lookup() {
        let r = core_registry();
        let k = KindName::new("smoke.cpu").unwrap();
        let info = r.get(&k).expect("smoke.cpu registered");
        assert_eq!(info.domain, DomainRequirement::Required);
        assert_eq!(info.quirks, &["trace"]);
    }

    #[test]
    fn registry_rejects_unknown() {
        let r = core_registry();
        assert!(r.get(&KindName::new("sm83.ppu").unwrap()).is_some());
        // The Z80 family registered with the debug probe group.
        let z80 = r.get(&KindName::new("z80.cpu").unwrap()).unwrap();
        assert_eq!(z80.quirks, &["undocumented"]);
        assert!(r.get(&KindName::new("nonexistent.kind").unwrap()).is_none());
    }

    #[test]
    fn nes_kinds_declare_quirks() {
        let r = core_registry();
        let cpu = r.get(&KindName::new("rp2A03.cpu").unwrap()).unwrap();
        assert!(cpu.quirks.contains(&"undocumented"));
        assert!(r.get(&KindName::new("rp2A07.ppu").unwrap()).is_some());
        assert!(r.get(&KindName::new("mos6502.cpu").unwrap()).is_some());
    }

    #[test]
    fn sm83_kinds_declare_quirks() {
        let r = core_registry();
        let cpu = r.get(&KindName::new("sm83.cpu").unwrap()).unwrap();
        assert!(cpu.quirks.contains(&"oam_bug"));
        assert!(cpu.quirks.contains(&"wave_ram_undoc"));
        assert!(cpu.quirks.contains(&"halt_undoc"));
    }
}
