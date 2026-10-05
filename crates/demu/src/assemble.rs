//! The machine assembly.
//!
//! The binary composes the device constructors of the machine
//! families: the Game Boy and Game Boy Color of the SM83 family, the
//! NES and the Apple II machines of the 6502 family, the 2600, 5200,
//! 800, and 7800 forms of the Atari and Commodore families, the SNES
//! and Apple IIgs of the 65C816 family, the Lynx of the 65SC02
//! family, the PC Engine of the HuC6280 family, and the Sega,
//! Sinclair, and TI-85 machines of the Z80 family. The configuration
//! selects kinds by name; this registry supplies the compiled
//! constructors.

use demu_config::model::DeviceInstance;
use demu_core::device::Device;
use demu_core::machine::{DeviceCtors, MachineError};
use demu_cpu_huc6280::CpuDevice as HucCpuDevice;
use demu_cpu_huc6280::Huc6280;
use demu_cpu_mos65sc02::CpuDevice as Sc02CpuDevice;
use demu_cpu_mos65sc02::Mos65Sc02;
use demu_cpu_sm83::CpuDevice as Sm83CpuDevice;
use demu_cpu_sm83::Sm83;
use demu_cpu_wdc65c816::CpuDevice as WdcCpuDevice;
use demu_cpu_wdc65c816::Wdc65c816;
use demu_cpu_z80::CpuDevice as Z80CpuDevice;
use demu_cpu_z80::Z80;
use demu_dev_audio::Apu;
use demu_dev_audio::IigsDoc;
use demu_dev_audio::NesApu;
use demu_dev_audio::PcePsg;
use demu_dev_audio::Sn76489;
use demu_dev_audio::apu::Apu as SnesApu;
use demu_dev_cartridge::Cart;
use demu_dev_cartridge::LynxCart;
use demu_dev_cartridge::NesCart;
use demu_dev_cartridge::PceCart;
use demu_dev_cartridge::SegaCart;
use demu_dev_cartridge::SnesCart;
use demu_dev_dma::{Hdma, OamDma, SnesSys};
use demu_dev_io::Iwm;
use demu_dev_io::LynxMikey;
use demu_dev_io::PceTurbotap;
use demu_dev_io::SegaIo;
use demu_dev_io::Ti85Mem;
use demu_dev_io::ZxUla;
use demu_dev_io::{Io, Joypad, Mega2, SnesPad};
use demu_dev_memory::StaticRam;
use demu_dev_memory::{Bootrom, Hram, Wram};
use demu_dev_memory::{LynxBootrom, LynxMapCtl};
use demu_dev_memory::{Z80BankRom, ZxBankRam};
use demu_dev_serial::Ti85Link;
use demu_dev_video::NesPpu;
use demu_dev_video::Ppu;
use demu_dev_video::SnesPpu;
use demu_dev_video::Ti85Lcd;
use demu_dev_video::Vdp;
use demu_dev_video::lynx::LynxSuzy;
use demu_dev_video::pce::{PceVce, PceVdc};

/// The device kind that carries the cartridge image.
pub const CART_KIND: &str = "sm83.cart";

/// The 6502 family cartridge kinds.
pub const NES_CART_KINDS: &[&str] = &["rp2A03.cart", "rp2A07.cart", "mos6502.cart"];

/// The Atari family cartridge kinds.
pub const ATARI_CART_KINDS: &[&str] = &["atari.cart2600", "atari.cart5200", "atari.cart7800"];

/// The SNES family cartridge kinds.
pub const SNES_CART_KINDS: &[&str] = &["snes.cart"];

/// The Z80 family cartridge kinds.
pub const SEGA_CART_KINDS: &[&str] = &["sega.cart"];

/// True for a device kind that carries the cartridge image.
pub fn is_cart_kind(kind: &str) -> bool {
    kind == CART_KIND
        || NES_CART_KINDS.contains(&kind)
        || ATARI_CART_KINDS.contains(&kind)
        || SNES_CART_KINDS.contains(&kind)
        || SEGA_CART_KINDS.contains(&kind)
}

/// The NES cartridge and pixel-processing-unit coupling: the mirror
/// mode from the iNES header reaches the PPU after the boot.
pub fn apply_nes_cartridge_wiring(machine: &mut demu_core::machine::Machine) {
    let name = machine
        .config
        .devices
        .iter()
        .find(|d| is_cart_kind(d.kind.as_str()))
        .map(|d| d.name.to_string());
    let Some(name) = name else { return };
    let mirroring = machinable_mirroring(machine, &name);
    if let Some(ppu) = machine
        .device_by_name_mut("ppu")
        .and_then(|d| d.as_any_mut().downcast_mut::<NesPpu>())
    {
        ppu.set_mirroring(mirroring);
    }
}

/// The mirror mode from the cartridge's parsed header, one after the
/// boot ran and the header parsed.
fn machinable_mirroring(machine: &demu_core::machine::Machine, name: &str) -> u8 {
    machine
        .device_by_name(name)
        .and_then(|d| {
            d.as_any()
                .downcast_ref::<demu_dev_cartridge::NesCart>()
                .map(|cart| cart.mirroring())
        })
        .unwrap_or(0)
}

/// Register the device constructors of all machine families.
pub fn demu_ctors() -> DeviceCtors {
    let mut ctors = DeviceCtors::new();
    ctors.register("sm83.cpu", Box::new(new_cpu));
    ctors.register("sm83.bootrom", Box::new(new_bootrom));
    ctors.register("sm83.cart", Box::new(new_cart));
    ctors.register("sm83.wram", Box::new(new_wram));
    ctors.register("sm83.hram", Box::new(new_hram));
    ctors.register("sm83.ppu", Box::new(new_ppu));
    ctors.register("sm83.apu", Box::new(new_apu));
    ctors.register("sm83.dma", Box::new(new_dma));
    ctors.register("sm83.hdma", Box::new(new_hdma));
    ctors.register("sm83.timer", Box::new(new_timer));
    ctors.register("sm83.joypad", Box::new(new_joypad));
    ctors.register("sm83.serial", Box::new(new_serial));
    ctors.register("sm83.io", Box::new(new_io));
    ctors.register("rp2A03.cpu", Box::new(new_nes_cpu));
    ctors.register("rp2A07.cpu", Box::new(new_nes_cpu));
    ctors.register("mos6502.cpu", Box::new(new_nes_cpu));
    ctors.register("rp2A03.wram", Box::new(new_nes_wram));
    ctors.register("rp2A07.wram", Box::new(new_nes_wram));
    ctors.register("rp2A03.cart", Box::new(new_nes_cart));
    ctors.register("rp2A07.cart", Box::new(new_nes_cart));
    ctors.register("mos6502.cart", Box::new(new_nes_cart));
    ctors.register("rp2A03.dma", Box::new(new_nes_dma));
    ctors.register("rp2A07.dma", Box::new(new_nes_dma));
    ctors.register("rp2A03.joypad", Box::new(new_nes_controller));
    ctors.register("rp2A07.joypad", Box::new(new_nes_controller));
    ctors.register("rp2A03.ppu", Box::new(new_nes_ppu));
    ctors.register("rp2A07.ppu", Box::new(new_nes_ppu));
    ctors.register("rp2A03.apu", Box::new(new_nes_apu));
    ctors.register("rp2A07.apu", Box::new(new_nes_apu));
    ctors.register("atari.tia", Box::new(new_atari_tia));
    ctors.register("atari.riot", Box::new(new_atari_riot));
    ctors.register("atari.antic", Box::new(new_atari_antic));
    ctors.register("atari.pokey", Box::new(new_atari_pokey));
    ctors.register("atari.maria", Box::new(new_atari_maria));
    ctors.register("atari.ram", Box::new(new_atari_ram));
    ctors.register("atari.rom", Box::new(new_atari_rom));
    ctors.register("atari.cart2600", Box::new(new_atari_cart2600));
    ctors.register("atari.cart5200", Box::new(new_atari_cart5200));
    ctors.register("atari.cart7800", Box::new(new_atari_cart7800));
    ctors.register("atari.pia", Box::new(new_atari_pia));
    ctors.register("c64.vic", Box::new(new_c64_vic));
    ctors.register("c64.sid", Box::new(new_c64_sid));
    ctors.register("c64.cia", Box::new(new_c64_cia));
    ctors.register("a2.video", Box::new(new_a2_video));
    ctors.register("a2.io", Box::new(new_a2_io));
    ctors.register("a2.disk2", Box::new(new_a2_disk2));
    ctors.register("wdc65c816.cpu", Box::new(new_wdc_cpu));
    ctors.register("snes.ppu", Box::new(new_snes_ppu));
    ctors.register("snes.sys", Box::new(new_snes_sys));
    ctors.register("snes.wram", Box::new(new_snes_wram));
    ctors.register("snes.pad", Box::new(new_snes_pad));
    ctors.register("snes.cart", Box::new(new_snes_cart));
    ctors.register("apu.core", Box::new(new_snes_apu));
    ctors.register("iigs.doc", Box::new(new_iigs_doc));
    ctors.register("iigs.mega2", Box::new(new_iigs_mega2));
    ctors.register("iigs.iwm", Box::new(new_iigs_iwm));
    ctors.register("vl65nc02.cpu", Box::new(new_vl65_cpu));
    ctors.register("mos65sc02.cpu", Box::new(new_sc02_cpu));
    ctors.register("huc6280.cpu", Box::new(new_huc_cpu));
    ctors.register("lynx.ram", Box::new(new_lynx_ram));
    ctors.register("lynx.bootrom", Box::new(new_lynx_bootrom));
    ctors.register("lynx.mapctl", Box::new(new_lynx_mapctl));
    ctors.register("lynx.mikey", Box::new(new_lynx_mikey));
    ctors.register("lynx.suzy", Box::new(new_lynx_suzy));
    ctors.register("lynx.cart", Box::new(new_lynx_cart));
    ctors.register("pce.vdc", Box::new(new_pce_vdc));
    ctors.register("pce.vce", Box::new(new_pce_vce));
    ctors.register("pce.psg", Box::new(new_pce_psg));
    ctors.register("pce.joypad", Box::new(new_pce_joypad));
    ctors.register("pce.ram", Box::new(new_pce_ram));
    ctors.register("pce.cart", Box::new(new_pce_cart));
    ctors.register("z80.cpu", Box::new(new_z80_cpu));
    ctors.register("z80.rom", Box::new(new_z80_rom));
    ctors.register("z80.ram", Box::new(new_z80_ram));
    ctors.register("z80.bankrom", Box::new(new_z80_bankrom));
    ctors.register("tms9918.vdp", Box::new(new_tms_vdp));
    ctors.register("sms.vdp", Box::new(new_sms_vdp));
    ctors.register("gg.vdp", Box::new(new_gg_vdp));
    ctors.register("sn76489.psg", Box::new(new_sn76489));
    ctors.register("sega.io", Box::new(new_sega_io));
    ctors.register("sega.cart", Box::new(new_sega_cart));
    ctors.register("zx.ula", Box::new(new_zx_ula));
    ctors.register("zx.bankram", Box::new(new_zx_bankram));
    ctors.register("ti85.mem", Box::new(new_ti85_mem));
    ctors.register("ti85.lcd", Box::new(new_ti85_lcd));
    ctors.register("ti85.link", Box::new(new_ti85_link));
    ctors
}

fn new_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Sm83CpuDevice::Sm83(Sm83::new(device, config))))
}

fn new_bootrom(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Bootrom::new(device)))
}

fn new_cart(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Cart::new(device)))
}

fn new_wram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Wram::new(device)))
}

fn new_hram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Hram::new(device)))
}

fn new_ppu(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Ppu::new(device)))
}

fn new_apu(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Apu::new(device)))
}

fn new_dma(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(OamDma::new(device)))
}

fn new_hdma(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Hdma::new(device)))
}

fn new_timer(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_io::Timer::new(device)))
}

fn new_joypad(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Joypad::new(device)))
}

fn new_serial(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_serial::Serial::new(device)))
}

fn new_io(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Io::new(device)))
}

// The 6502 device family: the NES.

fn new_nes_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_cpu_mos6502::CpuDevice::Mos6502(
        demu_cpu_mos6502::Mos6502::new(device, config),
    )))
}

fn new_nes_wram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Wram::new(device)))
}

fn new_nes_cart(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(NesCart::new(device)))
}

fn new_nes_dma(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_dma::NesOamDma::new(device)))
}

fn new_nes_controller(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_io::Controller::new(device)))
}

fn new_nes_ppu(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(NesPpu::new(device)))
}

fn new_nes_apu(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(NesApu::new(device)))
}

// The Atari device family: the 2600, the 5200, and the 7800.

fn new_atari_tia(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_video::Tia::new(device)))
}

fn new_atari_riot(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_io::Riot::new(device)))
}

fn new_atari_antic(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_video::Antic::new(device)))
}

fn new_atari_pokey(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_audio::Pokey::new(device)))
}

fn new_atari_maria(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_video::Maria::new(device)))
}

fn new_atari_ram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let mut ram = demu_dev_memory::StaticRam::new(device)
        .map_err(|e| MachineError::Assembly(e.to_string()))?;
    ram.set_readonly(false);
    Ok(Box::new(ram))
}

fn new_atari_rom(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let mut rom = demu_dev_memory::StaticRam::new(device)
        .map_err(|e| MachineError::Assembly(e.to_string()))?;
    rom.set_readonly(true);
    Ok(Box::new(rom))
}

fn new_atari_pia(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_io::Pia::new(device)))
}

fn new_c64_vic(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_video::Vic::new(device)))
}

fn new_c64_sid(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_audio::Sid::new(device)))
}

fn new_c64_cia(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_io::Cia::new(device)))
}

fn new_a2_video(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_video::AppleVideo::new(device)))
}

fn new_a2_io(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_io::AppleIo::new(device)))
}

fn new_a2_disk2(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_io::Disk2::new(device)))
}

fn new_atari_cart2600(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_cartridge::Atari2600Cart::new(device)))
}

fn new_atari_cart5200(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_cartridge::Atari5200Cart::new(device)))
}

fn new_atari_cart7800(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_cartridge::Atari7800Cart::new(device)))
}

// The 65C816 device families: the SNES and the Apple IIgs.

fn new_wdc_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(WdcCpuDevice::Wdc65c816(Wdc65c816::new(
        device, config,
    ))))
}

fn new_snes_ppu(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(SnesPpu::new(device)))
}

fn new_snes_sys(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(SnesSys::new(device)))
}

fn new_snes_wram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(demu_dev_memory::SnesWram::new(device)))
}

fn new_snes_pad(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(SnesPad::new(device)))
}

fn new_snes_cart(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(SnesCart::new(device)))
}

fn new_snes_apu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(SnesApu::new(device, config)))
}

fn new_iigs_doc(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(IigsDoc::new(device)))
}

fn new_iigs_mega2(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Mega2::new(device)))
}

fn new_iigs_iwm(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Iwm::new(device)))
}

// The 65SC02 device family: the Lynx machine slice and the plain CMOS
// core.

fn new_vl65_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Sc02CpuDevice::Vl65Nc02(Mos65Sc02::new(
        device, config,
    ))))
}

fn new_sc02_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Sc02CpuDevice::Mos65Sc02(Mos65Sc02::new(
        device, config,
    ))))
}

fn new_huc_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(HucCpuDevice::Huc6280(Huc6280::new(
        device, config,
    ))))
}

fn new_lynx_ram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let mut ram = demu_dev_memory::StaticRam::new(device)
        .map_err(|e| MachineError::Assembly(e.to_string()))?;
    ram.set_readonly(false);
    Ok(Box::new(ram))
}

fn new_lynx_bootrom(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(LynxBootrom::new(device)))
}

fn new_lynx_mapctl(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(LynxMapCtl::new(device)))
}

fn new_lynx_mikey(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(LynxMikey::new(device)))
}

fn new_lynx_suzy(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(LynxSuzy::new(device, config)))
}

fn new_lynx_cart(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(LynxCart::new(device)))
}

fn new_pce_vdc(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(PceVdc::new(device, config)))
}

fn new_pce_vce(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(PceVce::new(device)))
}

fn new_pce_psg(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(PcePsg::new(device)))
}

fn new_pce_joypad(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(PceTurbotap::new(device)))
}

fn new_pce_ram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let mut ram = demu_dev_memory::StaticRam::new(device)
        .map_err(|e| MachineError::Assembly(e.to_string()))?;
    ram.set_readonly(false);
    Ok(Box::new(ram))
}

fn new_pce_cart(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(PceCart::new(device)))
}

fn new_z80_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Z80CpuDevice::Z80(Z80::new(device, config))))
}

fn new_z80_rom(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let mut rom = StaticRam::new(device).map_err(|e| MachineError::Assembly(e.to_string()))?;
    rom.set_readonly(true);
    Ok(Box::new(rom))
}

fn new_z80_ram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let mut ram = StaticRam::new(device).map_err(|e| MachineError::Assembly(e.to_string()))?;
    ram.set_readonly(false);
    Ok(Box::new(ram))
}

fn new_z80_bankrom(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let rom = Z80BankRom::new(device).map_err(|e| MachineError::Assembly(e.to_string()))?;
    Ok(Box::new(rom))
}

fn new_tms_vdp(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Vdp::new(device, config)))
}

fn new_sms_vdp(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Vdp::new(device, config)))
}

fn new_gg_vdp(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Vdp::new(device, config)))
}

fn new_sn76489(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Sn76489::new(device)))
}

fn new_sega_io(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let io = SegaIo::new(device, config).map_err(|e| MachineError::Assembly(e.to_string()))?;
    Ok(Box::new(io))
}

fn new_sega_cart(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(SegaCart::new(device)))
}

fn new_zx_ula(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    let ula = ZxUla::new(device, config).map_err(|e| MachineError::Assembly(e.to_string()))?;
    Ok(Box::new(ula))
}

fn new_zx_bankram(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(ZxBankRam::new(device)))
}

fn new_ti85_mem(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Ti85Mem::new(device)))
}

fn new_ti85_lcd(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Ti85Lcd::new(device, config)))
}

fn new_ti85_link(
    device: &DeviceInstance,
    _config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(Ti85Link::new(device)))
}
