// Copyright (c) 2020 Huawei Technologies Co.,Ltd. All rights reserved.
//
// StratoVirt is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan
// PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//         http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
// KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
// NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

mod mch;
mod syscall;

use std::ops::Deref;
use std::os::unix::io::RawFd;
use std::sync::{Arc, Condvar, Mutex};

use address_space::{AddressSpace, GuestAddress, Region};
use boot_loader::{load_linux, BootLoaderConfig};
use cpu::{CPUBootConfig, CpuTopology, CPU};
use devices::legacy::{Serial, SERIAL_ADDR};
use kvm_bindings::{kvm_pit_config, KVM_PIT_SPEAKER_DUMMY};
use kvm_ioctls::{Kvm, VmFd};
use machine_manager::config::{
    BalloonConfig, BootSource, ConsoleConfig, DriveConfig, NetworkInterfaceConfig, SerialConfig,
    VmConfig, VsockConfig,
};
use machine_manager::event_loop::EventLoop;
use machine_manager::machine::{
    DeviceInterface, KvmVmState, MachineAddressInterface, MachineExternalInterface,
    MachineInterface, MachineLifecycle,
};
use machine_manager::qmp::{qmp_schema, QmpChannel, Response};
use pci::{PciDevOps, PciHost};
use sysbus::SysBus;
use util::loop_context::{EventLoopManager, EventNotifierHelper};
use util::seccomp::BpfRule;
use virtio::{qmp_balloon, qmp_query_balloon};
use vmm_sys_util::eventfd::EventFd;
use vmm_sys_util::terminal::Terminal;

use super::errors::{ErrorKind, Result};
use super::StdMachineOps;
use crate::errors::{ErrorKind as MachineErrorKind, Result as MachineResult};
use crate::MachineOps;
use mch::Mch;
use syscall::syscall_whitelist;

const VENDOR_ID_INTEL: u16 = 0x8086;

/// The type of memory layout entry on x86_64
#[allow(dead_code)]
#[cfg(target_arch = "x86_64")]
#[repr(usize)]
pub enum LayoutEntryType {
    MemBelow4g = 0_usize,
    PcieMmio,
    PcieEcam,
    Mmio,
    IoApic,
    LocalApic,
    MemAbove4g,
}

/// Layout of x86_64
#[cfg(target_arch = "x86_64")]
pub const MEM_LAYOUT: &[(u64, u64)] = &[
    (0, 0xC000_0000),                // MemBelow4g
    (0xB000_0000, 0x1000_0000),      // PcieEcam
    (0xC000_0000, 0x3000_0000),      // PcieMmio
    (0xF010_0000, 0x200),            // Mmio
    (0xFEC0_0000, 0x10_0000),        // IoApic
    (0xFEE0_0000, 0x10_0000),        // LocalApic
    (0x1_0000_0000, 0x80_0000_0000), // MemAbove4g
];

/// Standard machine structure.
pub struct StdMachine {
    /// `vCPU` topology, support sockets, cores, threads.
    cpu_topo: CpuTopology,
    /// `vCPU` devices.
    cpus: Vec<Arc<CPU>>,
    /// IO address space.
    sys_io: Arc<AddressSpace>,
    /// Memory address space.
    sys_mem: Arc<AddressSpace>,
    /// System bus.
    sysbus: SysBus,
    /// PCI/PCIe host bridge.
    pci_host: Arc<Mutex<PciHost>>,
    /// VM running state.
    vm_state: Arc<(Mutex<KvmVmState>, Condvar)>,
    /// Vm boot_source config.
    boot_source: Arc<Mutex<BootSource>>,
    /// VM power button, handle VM `Shutdown` event.
    power_button: EventFd,
}

impl StdMachine {
    #[allow(dead_code)]
    pub fn new(vm_config: &VmConfig) -> MachineResult<Self> {
        use crate::errors::ResultExt;

        let cpu_topo = CpuTopology::new(vm_config.machine_config.nr_cpus);
        let sys_io = AddressSpace::new(Region::init_container_region(1 << 16))
            .chain_err(|| MachineErrorKind::CrtMemSpaceErr)?;
        let sys_mem = AddressSpace::new(Region::init_container_region(u64::max_value()))
            .chain_err(|| MachineErrorKind::CrtIoSpaceErr)?;
        let sysbus = SysBus::new(
            &sys_io,
            &sys_mem,
            (5, 15),
            (
                MEM_LAYOUT[LayoutEntryType::Mmio as usize].0,
                MEM_LAYOUT[LayoutEntryType::Mmio as usize + 1].0,
            ),
        );
        // Machine state init
        let vm_state = Arc::new((Mutex::new(KvmVmState::Created), Condvar::new()));

        Ok(StdMachine {
            cpu_topo,
            cpus: Vec::new(),
            sys_io: sys_io.clone(),
            sys_mem: sys_mem.clone(),
            sysbus,
            pci_host: Arc::new(Mutex::new(PciHost::new(&sys_io, &sys_mem))),
            boot_source: Arc::new(Mutex::new(vm_config.clone().boot_source)),
            vm_state,
            power_button: EventFd::new(libc::EFD_NONBLOCK)
                .chain_err(|| MachineErrorKind::InitPwrBtnErr)?,
        })
    }

    /// Run `StdMachine` with `paused` flag.
    ///
    /// # Arguments
    ///
    /// * `paused` - Flag for `paused` when `StdMachine` starts to run.
    #[allow(dead_code)]
    pub fn run(&self, paused: bool) -> MachineResult<()> {
        <Self as MachineOps>::vm_start(paused, &self.cpus, &mut self.vm_state.0.lock().unwrap())
    }
}

impl StdMachineOps for StdMachine {
    fn init_pci_host(&self, vm_fd: &Arc<VmFd>) -> Result<()> {
        use super::errors::ResultExt;

        let root_bus = Arc::downgrade(&self.pci_host.lock().unwrap().root_bus);
        let mmconfig_region_ops = PciHost::build_mmconfig_ops(self.pci_host.clone());
        let mmconfig_region = Region::init_io_region(
            MEM_LAYOUT[LayoutEntryType::PcieEcam as usize].1,
            mmconfig_region_ops.clone(),
        );
        self.sys_mem
            .root()
            .add_subregion(
                mmconfig_region.clone(),
                MEM_LAYOUT[LayoutEntryType::PcieEcam as usize].0,
            )
            .chain_err(|| "Failed to register ECAM in memory space.")?;

        let pio_addr_ops = PciHost::build_pio_addr_ops(self.pci_host.clone());
        let pio_addr_region = Region::init_io_region(4, pio_addr_ops);
        self.sys_io
            .root()
            .add_subregion(pio_addr_region, 0xcf8)
            .chain_err(|| "Failed to register CONFIG_ADDR port in I/O space.")?;
        let pio_data_ops = PciHost::build_pio_data_ops(self.pci_host.clone());
        let pio_data_region = Region::init_io_region(4, pio_data_ops);
        self.sys_io
            .root()
            .add_subregion(pio_data_region, 0xcfc)
            .chain_err(|| "Failed to register CONFIG_DATA port in I/O space.")?;

        let mch = Mch::new(
            vm_fd.clone(),
            root_bus,
            mmconfig_region,
            mmconfig_region_ops,
        );
        PciDevOps::realize(mch, &vm_fd)?;
        Ok(())
    }
}

impl MachineOps for StdMachine {
    fn arch_ram_ranges(&self, mem_size: u64) -> Vec<(u64, u64)> {
        // ranges is the vector of (start_addr, size)
        let mut ranges = Vec::<(u64, u64)>::new();
        let gap_start = MEM_LAYOUT[LayoutEntryType::MemBelow4g as usize].0
            + MEM_LAYOUT[LayoutEntryType::MemBelow4g as usize].1;
        ranges.push((0, std::cmp::min(gap_start, mem_size)));
        if mem_size > gap_start {
            let gap_end = MEM_LAYOUT[LayoutEntryType::MemAbove4g as usize].0;
            ranges.push((gap_end, mem_size - gap_start));
        }

        ranges
    }

    fn init_interrupt_controller(
        &mut self,
        vm_fd: &Arc<VmFd>,
        _vcpu_count: u64,
    ) -> MachineResult<()> {
        use crate::errors::ResultExt;

        vm_fd
            .create_irq_chip()
            .chain_err(|| MachineErrorKind::CrtIrqchipErr)?;
        Ok(())
    }

    fn load_boot_source(&self) -> MachineResult<CPUBootConfig> {
        use crate::errors::ResultExt;

        let boot_source = self.boot_source.lock().unwrap();
        let initrd = boot_source.initrd.as_ref().map(|b| b.initrd_file.clone());

        let gap_start = MEM_LAYOUT[LayoutEntryType::MemBelow4g as usize].0
            + MEM_LAYOUT[LayoutEntryType::MemBelow4g as usize].1;
        let gap_end = MEM_LAYOUT[LayoutEntryType::MemAbove4g as usize].0;
        let bootloader_config = BootLoaderConfig {
            kernel: boot_source.kernel_file.clone(),
            initrd,
            kernel_cmdline: boot_source.kernel_cmdline.to_string(),
            cpu_count: self.cpu_topo.nrcpus,
            gap_range: (gap_start, gap_end - gap_start),
            ioapic_addr: MEM_LAYOUT[LayoutEntryType::IoApic as usize].0 as u32,
            lapic_addr: MEM_LAYOUT[LayoutEntryType::LocalApic as usize].0 as u32,
            prot64_mode: false,
        };
        let layout = load_linux(&bootloader_config, &self.sys_mem)
            .chain_err(|| MachineErrorKind::LoadKernErr)?;

        Ok(CPUBootConfig {
            prot64_mode: false,
            boot_ip: layout.boot_ip,
            boot_sp: layout.boot_sp,
            boot_selector: layout.boot_selector,
            zero_page: layout.zero_page_addr,
            code_segment: layout.segments.code_segment,
            data_segment: layout.segments.data_segment,
            gdt_base: layout.segments.gdt_base,
            gdt_size: layout.segments.gdt_limit,
            idt_base: layout.segments.idt_base,
            idt_size: layout.segments.idt_limit,
            pml4_start: layout.boot_pml4_addr,
        })
    }

    fn add_serial_device(&mut self, config: &SerialConfig, vm_fd: &Arc<VmFd>) -> MachineResult<()> {
        use crate::errors::ResultExt;

        let region_base: u64 = SERIAL_ADDR;
        let region_size: u64 = 8;
        let serial = Serial::realize(
            Serial::default(),
            &mut self.sysbus,
            region_base,
            region_size,
            vm_fd,
        )?;

        if config.stdio {
            EventLoop::update_event(EventNotifierHelper::internal_notifiers(serial), None)
                .chain_err(|| MachineErrorKind::RegNotifierErr)?;
        }
        Ok(())
    }

    fn add_block_device(&mut self, _config: &DriveConfig) -> MachineResult<()> {
        Ok(())
    }

    fn add_vsock_device(&mut self, _config: &VsockConfig, _vm_fd: &Arc<VmFd>) -> MachineResult<()> {
        Ok(())
    }

    fn add_net_device(
        &mut self,
        _config: &NetworkInterfaceConfig,
        _vm_fd: &Arc<VmFd>,
    ) -> MachineResult<()> {
        Ok(())
    }

    fn add_console_device(
        &mut self,
        _config: &ConsoleConfig,
        _vm_fd: &Arc<VmFd>,
    ) -> MachineResult<()> {
        Ok(())
    }

    fn add_balloon_device(
        &mut self,
        _config: &BalloonConfig,
        _vm_fd: &Arc<VmFd>,
    ) -> MachineResult<()> {
        Ok(())
    }

    fn add_devices(&mut self, vm_config: &VmConfig, vm_fd: &Arc<VmFd>) -> MachineResult<()> {
        use crate::errors::ResultExt;

        if let Some(serial) = vm_config.serial.as_ref() {
            self.add_serial_device(&serial, vm_fd)
                .chain_err(|| MachineErrorKind::AddDevErr("serial".to_string()))?;
        }

        if let Some(vsock) = vm_config.vsock.as_ref() {
            self.add_vsock_device(&vsock, vm_fd)
                .chain_err(|| MachineErrorKind::AddDevErr("vsock".to_string()))?;
        }

        if let Some(drives) = vm_config.drives.as_ref() {
            for drive in drives {
                self.add_block_device(&drive)
                    .chain_err(|| MachineErrorKind::AddDevErr("block".to_string()))?;
            }
        }

        if let Some(nets) = vm_config.nets.as_ref() {
            for net in nets {
                self.add_net_device(&net, vm_fd)
                    .chain_err(|| MachineErrorKind::AddDevErr("net".to_string()))?;
            }
        }

        if let Some(consoles) = vm_config.consoles.as_ref() {
            for console in consoles {
                self.add_console_device(&console, vm_fd)
                    .chain_err(|| MachineErrorKind::AddDevErr("console".to_string()))?;
            }
        }

        if let Some(balloon) = vm_config.balloon.as_ref() {
            self.add_balloon_device(balloon, vm_fd)
                .chain_err(|| MachineErrorKind::AddDevErr("balloon".to_string()))?;
        }

        Ok(())
    }

    fn syscall_whitelist(&self) -> Vec<BpfRule> {
        syscall_whitelist()
    }

    fn realize(
        vm: &Arc<Mutex<Self>>,
        vm_config: &VmConfig,
        fds: (Kvm, &Arc<VmFd>),
    ) -> MachineResult<()> {
        use crate::errors::ResultExt;

        let mut locked_vm = vm.lock().unwrap();
        let kvm_fd = fds.0;
        let vm_fd = fds.1;
        locked_vm.init_memory(
            (kvm_fd, vm_fd),
            &vm_config.machine_config.mem_config,
            &locked_vm.sys_io,
            &locked_vm.sys_mem,
        )?;

        locked_vm.init_interrupt_controller(&vm_fd, u64::from(vm_config.machine_config.nr_cpus))?;
        let nr_cpus = vm_config.machine_config.nr_cpus;
        let mut vcpu_fds = vec![];
        for cpu_id in 0..nr_cpus {
            vcpu_fds.push(Arc::new(vm_fd.create_vcpu(cpu_id)?));
        }

        locked_vm
            .init_pci_host(&vm_fd)
            .chain_err(|| ErrorKind::InitPCIeHostErr)?;
        locked_vm.add_devices(vm_config, &vm_fd)?;

        let boot_config = locked_vm.load_boot_source()?;
        locked_vm.cpus.extend(<Self as MachineOps>::init_vcpu(
            vm.clone(),
            vm_config.machine_config.nr_cpus,
            (&vm_fd, &vcpu_fds),
            &boot_config,
        )?);

        let mut pit_config = kvm_pit_config::default();
        pit_config.flags = KVM_PIT_SPEAKER_DUMMY;
        vm_fd
            .create_pit2(pit_config)
            .chain_err(|| MachineErrorKind::CrtPitErr)?;
        vm_fd
            .set_tss_address(0xfffb_d000 as usize)
            .chain_err(|| MachineErrorKind::SetTssErr)?;
        locked_vm
            .register_power_event(&locked_vm.power_button)
            .chain_err(|| MachineErrorKind::InitPwrBtnErr)?;
        Ok(())
    }
}

impl MachineLifecycle for StdMachine {
    fn pause(&self) -> bool {
        if self.notify_lifecycle(KvmVmState::Running, KvmVmState::Paused) {
            event!(STOP);
            true
        } else {
            false
        }
    }

    fn resume(&self) -> bool {
        if !self.notify_lifecycle(KvmVmState::Paused, KvmVmState::Running) {
            return false;
        }

        event!(RESUME);
        true
    }

    fn destroy(&self) -> bool {
        let vmstate = {
            let state = self.vm_state.deref().0.lock().unwrap();
            *state
        };

        if !self.notify_lifecycle(vmstate, KvmVmState::Shutdown) {
            return false;
        }

        self.power_button.write(1).unwrap();
        true
    }

    fn notify_lifecycle(&self, old: KvmVmState, new: KvmVmState) -> bool {
        <Self as MachineOps>::vm_state_transfer(
            &self.cpus,
            &mut self.vm_state.0.lock().unwrap(),
            old,
            new,
        )
        .is_ok()
    }
}

impl MachineAddressInterface for StdMachine {
    fn pio_in(&self, addr: u64, mut data: &mut [u8]) -> bool {
        // The function pit_calibrate_tsc() in kernel gets stuck if data read from
        // io-port 0x61 is not 0x20.
        // This problem only happens before Linux version 4.18 (fixed by 368a540e0)
        if addr == 0x61 {
            data[0] = 0x20;
            return true;
        }
        let length = data.len() as u64;
        self.sys_io
            .read(&mut data, GuestAddress(addr), length)
            .is_ok()
    }

    fn pio_out(&self, addr: u64, mut data: &[u8]) -> bool {
        let count = data.len() as u64;
        self.sys_io
            .write(&mut data, GuestAddress(addr), count)
            .is_ok()
    }

    fn mmio_read(&self, addr: u64, mut data: &mut [u8]) -> bool {
        let length = data.len() as u64;
        self.sys_mem
            .read(&mut data, GuestAddress(addr), length)
            .is_ok()
    }

    fn mmio_write(&self, addr: u64, mut data: &[u8]) -> bool {
        let count = data.len() as u64;
        self.sys_mem
            .write(&mut data, GuestAddress(addr), count)
            .is_ok()
    }
}

impl DeviceInterface for StdMachine {
    fn query_status(&self) -> Response {
        let vmstate = self.vm_state.deref().0.lock().unwrap();
        let qmp_state = match *vmstate {
            KvmVmState::Running => qmp_schema::StatusInfo {
                singlestep: false,
                running: true,
                status: qmp_schema::RunState::running,
            },
            KvmVmState::Paused => qmp_schema::StatusInfo {
                singlestep: false,
                running: true,
                status: qmp_schema::RunState::paused,
            },
            _ => Default::default(),
        };

        Response::create_response(serde_json::to_value(&qmp_state).unwrap(), None)
    }

    fn query_cpus(&self) -> Response {
        let mut cpu_vec: Vec<serde_json::Value> = Vec::new();
        for cpu_index in 0..self.cpu_topo.max_cpus {
            if self.cpu_topo.get_mask(cpu_index as usize) == 1 {
                let thread_id = self.cpus[cpu_index as usize].tid();
                let (socketid, coreid, threadid) = self.cpu_topo.get_topo(cpu_index as usize);
                let cpu_instance = qmp_schema::CpuInstanceProperties {
                    node_id: None,
                    socket_id: Some(socketid as isize),
                    core_id: Some(coreid as isize),
                    thread_id: Some(threadid as isize),
                };
                let cpu_info = qmp_schema::CpuInfo::x86 {
                    current: true,
                    qom_path: String::from("/machine/unattached/device[")
                        + &cpu_index.to_string()
                        + &"]".to_string(),
                    halted: false,
                    props: Some(cpu_instance),
                    CPU: cpu_index as isize,
                    thread_id: thread_id as isize,
                    x86: qmp_schema::CpuInfoX86 {},
                };
                cpu_vec.push(serde_json::to_value(cpu_info).unwrap());
            }
        }
        Response::create_response(cpu_vec.into(), None)
    }

    fn query_hotpluggable_cpus(&self) -> Response {
        Response::create_empty_response()
    }

    fn balloon(&self, value: u64) -> Response {
        if qmp_balloon(value) {
            return Response::create_empty_response();
        }
        Response::create_error_response(
            qmp_schema::QmpErrorClass::DeviceNotActive(
                "No balloon device has been activated".to_string(),
            ),
            None,
        )
    }

    fn query_balloon(&self) -> Response {
        if let Some(actual) = qmp_query_balloon() {
            let ret = qmp_schema::BalloonInfo { actual };
            return Response::create_response(serde_json::to_value(&ret).unwrap(), None);
        }
        Response::create_error_response(
            qmp_schema::QmpErrorClass::DeviceNotActive(
                "No balloon device has been activated".to_string(),
            ),
            None,
        )
    }

    fn device_add(
        &self,
        _id: String,
        _driver: String,
        _addr: Option<String>,
        _lun: Option<usize>,
    ) -> Response {
        Response::create_empty_response()
    }

    fn device_del(&self, _device_id: String) -> Response {
        Response::create_empty_response()
    }

    fn blockdev_add(
        &self,
        _node_name: String,
        _file: qmp_schema::FileOptions,
        _cache: Option<qmp_schema::CacheOptions>,
        _read_only: Option<bool>,
    ) -> Response {
        Response::create_empty_response()
    }

    fn netdev_add(&self, _id: String, _if_name: Option<String>, _fds: Option<String>) -> Response {
        Response::create_empty_response()
    }

    fn getfd(&self, fd_name: String, if_fd: Option<RawFd>) -> Response {
        if let Some(fd) = if_fd {
            QmpChannel::set_fd(fd_name, fd);
            Response::create_empty_response()
        } else {
            let err_resp =
                qmp_schema::QmpErrorClass::GenericError("Invalid SCM message".to_string());
            Response::create_error_response(err_resp, None)
        }
    }
}

impl MachineInterface for StdMachine {}
impl MachineExternalInterface for StdMachine {}

impl EventLoopManager for StdMachine {
    fn loop_should_exit(&self) -> bool {
        let vmstate = self.vm_state.deref().0.lock().unwrap();
        *vmstate == KvmVmState::Shutdown
    }

    fn loop_cleanup(&self) -> util::errors::Result<()> {
        if let Err(e) = std::io::stdin().lock().set_canon_mode() {
            error!(
                "destroy virtual machine: reset stdin to canonical mode failed, {}",
                e
            );
        }
        Ok(())
    }
}
