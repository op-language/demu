//! The SM83 machine assembly.
//!
//! The binary composes the device constructors of the SM83 machine
//! family: the Game Boy and the Game Boy Color. The configuration
//! selects kinds by name; this registry supplies the compiled
//! constructors.

use demu_config::model::DeviceInstance;
use demu_core::device::Device;
use demu_core::machine::{DeviceCtors, MachineError};
use demu_cpu_sm83::CpuDevice;
use demu_dev_audio::Apu;
use demu_dev_cartridge::Cart;
use demu_dev_dma::{Hdma, OamDma};
use demu_dev_io::{Io, Joypad};
use demu_dev_memory::{Bootrom, Hram, Wram};
use demu_dev_video::Ppu;

/// The device kind that carries the cartridge image.
pub const CART_KIND: &str = "sm83.cart";

/// Register the SM83 device constructors.
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
    ctors
}

fn new_cpu(
    device: &DeviceInstance,
    config: &demu_config::model::SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    Ok(Box::new(CpuDevice::Sm83(demu_cpu_sm83::Sm83::new(
        device, config,
    ))))
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
