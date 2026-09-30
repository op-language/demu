//! The machine assembly.
//!
//! The binary composes the device constructors of the machine
//! families: the Game Boy and Game Boy Color of the SM83 family, the
//! NES of the 6502 family, and the 2600, 5200, and 7800 of the Atari
//! family. The configuration selects kinds by name; this registry
//! supplies the compiled constructors.

use demu_config::model::DeviceInstance;
use demu_core::device::Device;
use demu_core::machine::{DeviceCtors, MachineError};
use demu_cpu_sm83::CpuDevice as Sm83CpuDevice;
use demu_cpu_sm83::Sm83;
use demu_dev_audio::Apu;
use demu_dev_audio::NesApu;
use demu_dev_cartridge::Cart;
use demu_dev_cartridge::NesCart;
use demu_dev_dma::{Hdma, OamDma};
use demu_dev_io::{Io, Joypad};
use demu_dev_memory::{Bootrom, Hram, Wram};
use demu_dev_video::NesPpu;
use demu_dev_video::Ppu;

/// The device kind that carries the cartridge image.
pub const CART_KIND: &str = "sm83.cart";

/// The 6502 family cartridge kinds.
pub const NES_CART_KINDS: &[&str] = &["rp2A03.cart", "rp2A07.cart", "mos6502.cart"];

/// The Atari family cartridge kinds.
pub const ATARI_CART_KINDS: &[&str] = &["atari.cart2600", "atari.cart5200", "atari.cart7800"];

/// True for a device kind that carries the cartridge image.
pub fn is_cart_kind(kind: &str) -> bool {
    kind == CART_KIND || NES_CART_KINDS.contains(&kind) || ATARI_CART_KINDS.contains(&kind)
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
