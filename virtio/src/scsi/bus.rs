// Copyright (c) 2022 Huawei Technologies Co.,Ltd. All rights reserved.
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

use std::cmp;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::sync::{Arc, Mutex, Weak};

use anyhow::{anyhow, bail, Context, Result};

use crate::ScsiCntlr::{
    ScsiCntlr, ScsiCompleteCb, ScsiXferMode, VirtioScsiCmdReq, VirtioScsiCmdResp,
    VirtioScsiRequest, VIRTIO_SCSI_CDB_DEFAULT_SIZE, VIRTIO_SCSI_S_OK,
};
use crate::ScsiDisk::{
    ScsiDevice, DEFAULT_SECTOR_SIZE, SCSI_DISK_F_DPOFUA, SCSI_DISK_F_REMOVABLE, SCSI_TYPE_DISK,
    SCSI_TYPE_ROM,
};
use address_space::AddressSpace;
use byteorder::{BigEndian, ByteOrder};
use log::{debug, error, info};
use util::aio::{Aio, AioCb, IoCmd, Iovec};

/// Scsi Operation code.
pub const TEST_UNIT_READY: u8 = 0x00;
pub const REWIND: u8 = 0x01;
pub const REQUEST_SENSE: u8 = 0x03;
pub const FORMAT_UNIT: u8 = 0x04;
pub const READ_BLOCK_LIMITS: u8 = 0x05;
pub const INITIALIZE_ELEMENT_STATUS: u8 = 0x07;
pub const REASSIGN_BLOCKS: u8 = 0x07;
pub const READ_6: u8 = 0x08;
pub const WRITE_6: u8 = 0x0a;
pub const SET_CAPACITY: u8 = 0x0b;
pub const READ_REVERSE: u8 = 0x0f;
pub const WRITE_FILEMARKS: u8 = 0x10;
pub const SPACE: u8 = 0x11;
pub const INQUIRY: u8 = 0x12;
pub const RECOVER_BUFFERED_DATA: u8 = 0x14;
pub const MODE_SELECT: u8 = 0x15;
pub const RESERVE: u8 = 0x16;
pub const RELEASE: u8 = 0x17;
pub const COPY: u8 = 0x18;
pub const ERASE: u8 = 0x19;
pub const MODE_SENSE: u8 = 0x1a;
pub const LOAD_UNLOAD: u8 = 0x1b;
pub const SCAN: u8 = 0x1b;
pub const START_STOP: u8 = 0x1b;
pub const RECEIVE_DIAGNOSTIC: u8 = 0x1c;
pub const SEND_DIAGNOSTIC: u8 = 0x1d;
pub const ALLOW_MEDIUM_REMOVAL: u8 = 0x1e;
pub const SET_WINDOW: u8 = 0x24;
pub const READ_CAPACITY_10: u8 = 0x25;
pub const GET_WINDOW: u8 = 0x25;
pub const READ_10: u8 = 0x28;
pub const WRITE_10: u8 = 0x2a;
pub const SEND: u8 = 0x2a;
pub const SEEK_10: u8 = 0x2b;
pub const LOCATE_10: u8 = 0x2b;
pub const POSITION_TO_ELEMENT: u8 = 0x2b;
pub const WRITE_VERIFY_10: u8 = 0x2e;
pub const VERIFY_10: u8 = 0x2f;
pub const SEARCH_HIGH: u8 = 0x30;
pub const SEARCH_EQUAL: u8 = 0x31;
pub const OBJECT_POSITION: u8 = 0x31;
pub const SEARCH_LOW: u8 = 0x32;
pub const SET_LIMITS: u8 = 0x33;
pub const PRE_FETCH: u8 = 0x34;
pub const READ_POSITION: u8 = 0x34;
pub const GET_DATA_BUFFER_STATUS: u8 = 0x34;
pub const SYNCHRONIZE_CACHE: u8 = 0x35;
pub const LOCK_UNLOCK_CACHE: u8 = 0x36;
pub const INITIALIZE_ELEMENT_STATUS_WITH_RANGE: u8 = 0x37;
pub const READ_DEFECT_DATA: u8 = 0x37;
pub const MEDIUM_SCAN: u8 = 0x38;
pub const COMPARE: u8 = 0x39;
pub const COPY_VERIFY: u8 = 0x3a;
pub const WRITE_BUFFER: u8 = 0x3b;
pub const READ_BUFFER: u8 = 0x3c;
pub const UPDATE_BLOCK: u8 = 0x3d;
pub const READ_LONG_10: u8 = 0x3e;
pub const WRITE_LONG_10: u8 = 0x3f;
pub const CHANGE_DEFINITION: u8 = 0x40;
pub const WRITE_SAME_10: u8 = 0x41;
pub const UNMAP: u8 = 0x42;
pub const READ_TOC: u8 = 0x43;
pub const REPORT_DENSITY_SUPPORT: u8 = 0x44;
pub const GET_CONFIGURATION: u8 = 0x46;
pub const SANITIZE: u8 = 0x48;
pub const GET_EVENT_STATUS_NOTIFICATION: u8 = 0x4a;
pub const LOG_SELECT: u8 = 0x4c;
pub const LOG_SENSE: u8 = 0x4d;
pub const READ_DISC_INFORMATION: u8 = 0x51;
pub const RESERVE_TRACK: u8 = 0x53;
pub const MODE_SELECT_10: u8 = 0x55;
pub const RESERVE_10: u8 = 0x56;
pub const RELEASE_10: u8 = 0x57;
pub const MODE_SENSE_10: u8 = 0x5a;
pub const SEND_CUE_SHEET: u8 = 0x5d;
pub const PERSISTENT_RESERVE_IN: u8 = 0x5e;
pub const PERSISTENT_RESERVE_OUT: u8 = 0x5f;
pub const VARLENGTH_CDB: u8 = 0x7f;
pub const WRITE_FILEMARKS_16: u8 = 0x80;
pub const READ_REVERSE_16: u8 = 0x81;
pub const ALLOW_OVERWRITE: u8 = 0x82;
pub const EXTENDED_COPY: u8 = 0x83;
pub const ATA_PASSTHROUGH_16: u8 = 0x85;
pub const ACCESS_CONTROL_IN: u8 = 0x86;
pub const ACCESS_CONTROL_OUT: u8 = 0x87;
pub const READ_16: u8 = 0x88;
pub const COMPARE_AND_WRITE: u8 = 0x89;
pub const WRITE_16: u8 = 0x8a;
pub const WRITE_VERIFY_16: u8 = 0x8e;
pub const VERIFY_16: u8 = 0x8f;
pub const PRE_FETCH_16: u8 = 0x90;
pub const SPACE_16: u8 = 0x91;
pub const SYNCHRONIZE_CACHE_16: u8 = 0x91;
pub const LOCATE_16: u8 = 0x92;
pub const WRITE_SAME_16: u8 = 0x93;
pub const ERASE_16: u8 = 0x93;
pub const SERVICE_ACTION_IN_16: u8 = 0x9e;
pub const WRITE_LONG_16: u8 = 0x9f;
pub const REPORT_LUNS: u8 = 0xa0;
pub const ATA_PASSTHROUGH_12: u8 = 0xa1;
pub const MAINTENANCE_IN: u8 = 0xa3;
pub const MAINTENANCE_OUT: u8 = 0xa4;
pub const MOVE_MEDIUM: u8 = 0xa5;
pub const EXCHANGE_MEDIUM: u8 = 0xa6;
pub const SET_READ_AHEAD: u8 = 0xa7;
pub const READ_12: u8 = 0xa8;
pub const WRITE_12: u8 = 0xaa;
pub const SERVICE_ACTION_IN_12: u8 = 0xab;
pub const ERASE_12: u8 = 0xac;
pub const READ_DVD_STRUCTURE: u8 = 0xad;
pub const WRITE_VERIFY_12: u8 = 0xae;
pub const VERIFY_12: u8 = 0xaf;
pub const SEARCH_HIGH_12: u8 = 0xb0;
pub const SEARCH_EQUAL_12: u8 = 0xb1;
pub const SEARCH_LOW_12: u8 = 0xb2;
pub const READ_ELEMENT_STATUS: u8 = 0xb8;
pub const SEND_VOLUME_TAG: u8 = 0xb6;
pub const READ_DEFECT_DATA_12: u8 = 0xb7;
pub const SET_CD_SPEED: u8 = 0xbb;
pub const MECHANISM_STATUS: u8 = 0xbd;
pub const READ_CD: u8 = 0xbe;
pub const SEND_DVD_STRUCTURE: u8 = 0xbf;

/// SAM Status codes.
pub const GOOD: u8 = 0x00;
pub const CHECK_CONDITION: u8 = 0x02;
pub const CONDITION_GOOD: u8 = 0x04;
pub const BUSY: u8 = 0x08;
pub const INTERMEDIATE_GOOD: u8 = 0x10;
pub const INTERMEDIATE_C_GOOD: u8 = 0x14;
pub const RESERVATION_CONFLICT: u8 = 0x18;
pub const COMMAND_TERMINATED: u8 = 0x22;
pub const TASK_SET_FULL: u8 = 0x28;
pub const ACA_ACTIVE: u8 = 0x30;
pub const TASK_ABORTED: u8 = 0x40;

pub const STATUS_MASK: u8 = 0x3e;

pub const SCSI_CMD_BUF_SIZE: usize = 16;
pub const SCSI_SENSE_BUF_SIZE: usize = 252;

/// SERVICE ACTION IN subcodes.
pub const SUBCODE_READ_CAPACITY_16: u8 = 0x10;

/// Used to compute the number of sectors.
const SECTOR_SHIFT: u8 = 9;
/// Size of a sector of the block device.
const SECTOR_SIZE: u64 = (0x01_u64) << SECTOR_SHIFT;

const SCSI_DEFAULT_BLOCK_SIZE: i32 = 512;

/// Sense Keys.
pub const NO_SENSE: u8 = 0x00;
pub const RECOVERED_ERROR: u8 = 0x01;
pub const NOT_READY: u8 = 0x02;
pub const MEDIUM_ERROR: u8 = 0x03;
pub const HARDWARE_ERROR: u8 = 0x04;
pub const ILLEGAL_REQUEST: u8 = 0x05;
pub const UNIT_ATTENTION: u8 = 0x06;
pub const DATA_PROTECT: u8 = 0x07;
pub const BLANK_CHECK: u8 = 0x08;
pub const COPY_ABORTED: u8 = 0x0a;
pub const ABORTED_COMMAND: u8 = 0x0b;
pub const VOLUME_OVERFLOW: u8 = 0x0d;
pub const MISCOMPARE: u8 = 0x0e;

macro_rules! scsisense {
    ( $key:expr, $asc: expr, $ascq:expr) => {
        ScsiSense {
            key: $key,
            asc: $asc,
            ascq: $ascq,
        }
    };
}

/// Sense Code.
pub const SCSI_SENSE_NO_SENSE: ScsiSense = scsisense!(NO_SENSE, 0x00, 0x00);
pub const SCSI_SENSE_LUN_NOT_READY: ScsiSense = scsisense!(NOT_READY, 0x04, 0x03);
pub const SCSI_SENSE_NO_MEDIUM: ScsiSense = scsisense!(NOT_READY, 0x3a, 0x00);
pub const SCSI_SENSE_NOT_READY_REMOVAL_PREVENTED: ScsiSense = scsisense!(NOT_READY, 0x53, 0x02);
pub const SCSI_SENSE_TARGET_FAILURE: ScsiSense = scsisense!(HARDWARE_ERROR, 0x44, 0x00);
pub const SCSI_SENSE_INVALID_OPCODE: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x20, 0x00);
pub const SCSI_SENSE_LBA_OUT_OF_RANGE: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x21, 0x00);
pub const SCSI_SENSE_INVALID_FIELD: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x24, 0x00);
pub const SCSI_SENSE_INVALID_PARAM: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x26, 0x00);
pub const SCSI_SENSE_INVALID_PARAM_VALUE: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x26, 0x01);
pub const SCSI_SENSE_INVALID_PARAM_LEN: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x1a, 0x00);
pub const SCSI_SENSE_LUN_NOT_SUPPORTED: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x25, 0x00);
pub const SCSI_SENSE_SAVING_PARAMS_NOT_SUPPORTED: ScsiSense =
    scsisense!(ILLEGAL_REQUEST, 0x39, 0x00);
pub const SCSI_SENSE_INCOMPATIBLE_FORMAT: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x30, 0x00);
pub const SCSI_SENSE_ILLEGAL_REQ_REMOVAL_PREVENTED: ScsiSense =
    scsisense!(ILLEGAL_REQUEST, 0x53, 0x02);
pub const SCSI_SENSE_INVALID_TAG: ScsiSense = scsisense!(ILLEGAL_REQUEST, 0x4b, 0x01);
pub const SCSI_SENSE_IO_ERROR: ScsiSense = scsisense!(ABORTED_COMMAND, 0x00, 0x06);
pub const SCSI_SENSE_I_T_NEXUS_LOSS: ScsiSense = scsisense!(ABORTED_COMMAND, 0x29, 0x07);
pub const SCSI_SENSE_LUN_FAILURE: ScsiSense = scsisense!(ABORTED_COMMAND, 0x3e, 0x01);
pub const SCSI_SENSE_OVERLAPPED_COMMANDS: ScsiSense = scsisense!(ABORTED_COMMAND, 0x4e, 0x00);
pub const SCSI_SENSE_LUN_COMM_FAILURE: ScsiSense = scsisense!(ABORTED_COMMAND, 0x08, 0x00);
pub const SCSI_SENSE_LUN_NOT_RESPONDING: ScsiSense = scsisense!(ABORTED_COMMAND, 0x05, 0x00);
pub const SCSI_SENSE_COMMAND_TIMEOUT: ScsiSense = scsisense!(ABORTED_COMMAND, 0x2e, 0x02);
pub const SCSI_SENSE_COMMAND_ABORTED: ScsiSense = scsisense!(ABORTED_COMMAND, 0x2f, 0x02);
pub const SCSI_SENSE_READ_ERROR: ScsiSense = scsisense!(MEDIUM_ERROR, 0x11, 0x00);
pub const SCSI_SENSE_NOT_READY: ScsiSense = scsisense!(NOT_READY, 0x04, 0x00);
pub const SCSI_SENSE_CAPACITY_CHANGED: ScsiSense = scsisense!(UNIT_ATTENTION, 0x2a, 0x09);
pub const SCSI_SENSE_RESET: ScsiSense = scsisense!(UNIT_ATTENTION, 0x29, 0x00);
pub const SCSI_SENSE_SCSI_BUS_RESET: ScsiSense = scsisense!(UNIT_ATTENTION, 0x29, 0x02);
pub const SCSI_SENSE_UNIT_ATTENTION_NO_MEDIUM: ScsiSense = scsisense!(UNIT_ATTENTION, 0x3a, 0x00);
pub const SCSI_SENSE_MEDIUM_CHANGED: ScsiSense = scsisense!(UNIT_ATTENTION, 0x28, 0x00);
pub const SCSI_SENSE_REPORTED_LUNS_CHANGED: ScsiSense = scsisense!(UNIT_ATTENTION, 0x3f, 0x0e);
pub const SCSI_SENSE_DEVICE_INTERNAL_RESET: ScsiSense = scsisense!(UNIT_ATTENTION, 0x29, 0x04);
pub const SCSI_SENSE_WRITE_PROTECTED: ScsiSense = scsisense!(DATA_PROTECT, 0x27, 0x00);
pub const SCSI_SENSE_SPACE_ALLOC_FAILED: ScsiSense = scsisense!(DATA_PROTECT, 0x27, 0x07);

#[derive(Default)]
pub struct ScsiSense {
    key: u8,
    asc: u8,
    ascq: u8,
}

pub const SCSI_SENSE_LEN: u32 = 18;

/// Mode page codes for mode sense/set.
pub const MODE_PAGE_R_W_ERROR: u8 = 0x01;
pub const MODE_PAGE_HD_GEOMETRY: u8 = 0x04;
pub const MODE_PAGE_FLEXIBLE_DISK_GEOMETRY: u8 = 0x05;
pub const MODE_PAGE_CACHING: u8 = 0x08;
pub const MODE_PAGE_AUDIO_CTL: u8 = 0x0e;
pub const MODE_PAGE_POWER: u8 = 0x1a;
pub const MODE_PAGE_FAULT_FAIL: u8 = 0x1c;
pub const MODE_PAGE_TO_PROTECT: u8 = 0x1d;
pub const MODE_PAGE_CAPABILITIES: u8 = 0x2a;
pub const MODE_PAGE_ALLS: u8 = 0x3f;

pub const SCSI_MAX_INQUIRY_LEN: u32 = 256;
pub const SCSI_INQUIRY_PRODUCT_MAX_LEN: usize = 16;
pub const SCSI_INQUIRY_VENDOR_MAX_LEN: usize = 8;
pub const SCSI_INQUIRY_VERSION_MAX_LEN: usize = 4;
pub const SCSI_INQUIRY_VPD_SERIAL_NUMBER_MAX_LEN: usize = 32;

const SCSI_TARGET_INQUIRY_LEN: u32 = 36;

/// |     bit7 - bit 5     |     bit 4 - bit 0      |
/// | Peripheral Qualifier | Peripheral Device Type |
/// Unknown or no device type.
const TYPE_UNKNOWN: u8 = 0x1f;
/// A peripheral device having the specified peripheral device type is not connected to this logical unit.
const TYPE_INACTIVE: u8 = 0x20;
/// Scsi target device is not capable of supporting a peripheral device connected to this logical unit.
const TYPE_NO_LUN: u8 = 0x7f;

pub struct ScsiBus {
    /// Bus name.
    pub name: String,
    /// Scsi Devices attached to the bus.
    pub devices: HashMap<(u8, u16), Arc<Mutex<ScsiDevice>>>,
    /// Scsi Controller which the bus orignates from.
    pub parent_cntlr: Weak<Mutex<ScsiCntlr>>,
}

impl ScsiBus {
    pub fn new(bus_name: String, parent_cntlr: Weak<Mutex<ScsiCntlr>>) -> ScsiBus {
        ScsiBus {
            name: bus_name,
            devices: HashMap::new(),
            parent_cntlr,
        }
    }

    /// Get device by the target number and the lun number.
    /// If the device requested by the target number and the lun number is non-existen,
    /// return the first device in ScsiBus's devices list. It's OK because we will not
    /// use this "random" device, we will just use it to prove that the target is existen.
    pub fn get_device(&self, target: u8, lun: u16) -> Option<Arc<Mutex<ScsiDevice>>> {
        if let Some(dev) = self.devices.get(&(target, lun)) {
            return Some((*dev).clone());
        }

        // If lun device requested in CDB's LUNS bytes is not found, it may be a target request.
        // Target request means if there is any lun in this scsi target, it will response some
        // scsi commands. And, if there is no lun found in this scsi target, it means such target
        // is non-existent. So, we should find if there exists a lun which has the same id with
        // target id in CBD's LUNS bytes. And, if there exist two or more luns which have the same
        // target id, just return the first one is OK enough.
        for (id, device) in self.devices.iter() {
            let (target_id, lun_id) = id;
            if *target_id == target {
                debug!(
                    "Target request, target {}, requested lun {}, found lun {}",
                    target_id, lun, lun_id
                );
                return Some((*device).clone());
            }
        }

        // No lun found in requested target. It seems there is no such target requested in
        // CDB's LUNS bytes.
        debug!("Can't find scsi device target {} lun {}", target, lun);
        None
    }

    pub fn scsi_bus_parse_req_cdb(
        &self,
        cdb: [u8; VIRTIO_SCSI_CDB_DEFAULT_SIZE],
    ) -> Option<ScsiCommand> {
        let buf: [u8; SCSI_CMD_BUF_SIZE] = (cdb[0..SCSI_CMD_BUF_SIZE])
            .try_into()
            .expect("incorrect length");
        let command = cdb[0];
        let len = scsi_cdb_length(&cdb);
        if len < 0 {
            return None;
        }

        let xfer = scsi_cdb_xfer(&cdb);
        if xfer < 0 {
            return None;
        }

        let lba = scsi_cdb_lba(&cdb);
        if lba < 0 {
            return None;
        }

        Some(ScsiCommand {
            buf,
            command,
            len: len as u32,
            xfer: xfer as u32,
            lba: lba as u64,
            mode: scsi_cdb_xfer_mode(&cdb),
        })
    }
}

pub fn create_scsi_bus(bus_name: &str, scsi_cntlr: &Arc<Mutex<ScsiCntlr>>) -> Result<()> {
    let mut locked_scsi_cntlr = scsi_cntlr.lock().unwrap();
    let bus = ScsiBus::new(bus_name.to_string(), Arc::downgrade(scsi_cntlr));
    locked_scsi_cntlr.bus = Some(Arc::new(Mutex::new(bus)));
    Ok(())
}

#[derive(Clone)]
pub struct ScsiCommand {
    /// The Command Descriptor Block(CDB).
    pub buf: [u8; SCSI_CMD_BUF_SIZE],
    /// Scsi Operation Code.
    pub command: u8,
    /// Length of CDB.
    pub len: u32,
    /// Transfer length.
    pub xfer: u32,
    /// Logical Block Address.
    pub lba: u64,
    /// Transfer direction.
    mode: ScsiXferMode,
}

#[derive(Clone)]
pub struct ScsiRequest {
    cmd: ScsiCommand,
    _sense: [u8; SCSI_SENSE_BUF_SIZE],
    _sense_size: u32,
    _resid: u32,
    pub opstype: u32,
    pub virtioscsireq: Arc<Mutex<VirtioScsiRequest<VirtioScsiCmdReq, VirtioScsiCmdResp>>>,
    dev: Arc<Mutex<ScsiDevice>>,
}

impl ScsiRequest {
    pub fn new(
        req: Arc<Mutex<VirtioScsiRequest<VirtioScsiCmdReq, VirtioScsiCmdResp>>>,
        scsibus: Arc<Mutex<ScsiBus>>,
        scsidevice: Arc<Mutex<ScsiDevice>>,
    ) -> Result<Self> {
        if let Some(cmd) = scsibus
            .lock()
            .unwrap()
            .scsi_bus_parse_req_cdb(req.lock().unwrap().req.cdb)
        {
            let ops = cmd.command;
            let opstype = scsi_operation_type(ops);
            let _resid = cmd.xfer;

            Ok(ScsiRequest {
                cmd,
                _sense: [0; SCSI_SENSE_BUF_SIZE],
                _sense_size: 0,
                _resid,
                opstype,
                virtioscsireq: req.clone(),
                dev: scsidevice,
            })
        } else {
            bail!("Error CDB!");
        }
    }

    pub fn execute(
        &self,
        aio: &mut Box<Aio<ScsiCompleteCb>>,
        disk: &File,
        direct: bool,
        last_aio: bool,
        iocompletecb: ScsiCompleteCb,
    ) -> Result<u32> {
        let mut aiocb = AioCb {
            last_aio,
            file_fd: disk.as_raw_fd(),
            opcode: IoCmd::Noop,
            iovec: Vec::new(),
            offset: (self.cmd.lba << 9) as usize,
            process: true,
            iocb: None,
            iocompletecb,
        };

        for iov in self.virtioscsireq.lock().unwrap().iovec.iter() {
            let iovec = Iovec {
                iov_base: iov.iov_base,
                iov_len: iov.iov_len,
            };
            aiocb.iovec.push(iovec);
        }

        match self.cmd.mode {
            ScsiXferMode::ScsiXferFromDev => {
                aiocb.opcode = IoCmd::Preadv;
                if direct {
                    (*aio)
                        .as_mut()
                        .rw_aio(aiocb, SECTOR_SIZE)
                        .with_context(|| {
                            "Failed to process scsi request for reading asynchronously"
                        })?;
                } else {
                    (*aio).as_mut().rw_sync(aiocb).with_context(|| {
                        "Failed to process scsi request for reading synchronously"
                    })?;
                }
            }
            ScsiXferMode::ScsiXferToDev => {
                aiocb.opcode = IoCmd::Pwritev;
                if direct {
                    (*aio)
                        .as_mut()
                        .rw_aio(aiocb, SECTOR_SIZE)
                        .with_context(|| {
                            "Failed to process block request for writing asynchronously"
                        })?;
                } else {
                    (*aio).as_mut().rw_sync(aiocb).with_context(|| {
                        "Failed to process block request for writing synchronously"
                    })?;
                }
            }
            _ => {
                info!("xfer none");
            }
        }
        Ok(0)
    }

    pub fn emulate_execute(
        &self,
        iocompletecb: ScsiCompleteCb,
        req_lun_id: u16,
        found_lun_id: u16,
    ) -> Result<()> {
        debug!("scsi command is {:#x}", self.cmd.command);
        let mut not_supported_flag = false;
        let mut sense = None;
        let result;

        // Requested lun id is not equal to found device id means it may be a target request.
        // REPORT LUNS is also a target request command.
        if req_lun_id != found_lun_id || self.cmd.command == REPORT_LUNS {
            result = match self.cmd.command {
                REPORT_LUNS => scsi_command_emulate_report_luns(&self.cmd, &self.dev),
                INQUIRY => scsi_command_emulate_target_inquiry(req_lun_id, &self.cmd),
                REQUEST_SENSE => {
                    if req_lun_id != 0 {
                        sense = Some(SCSI_SENSE_LUN_NOT_SUPPORTED);
                    }
                    // Scsi Device does not realize sense buffer now, so just return.
                    Ok(Vec::new())
                }
                TEST_UNIT_READY => Ok(Vec::new()),
                _ => {
                    not_supported_flag = true;
                    sense = Some(SCSI_SENSE_INVALID_OPCODE);
                    Err(anyhow!("Invalid emulation target scsi command"))
                }
            };
        } else {
            // It's not a target request.
            result = match self.cmd.command {
                REQUEST_SENSE => {
                    sense = Some(SCSI_SENSE_NO_SENSE);
                    Ok(Vec::new())
                }
                WRITE_SAME_10 | WRITE_SAME_16 | SYNCHRONIZE_CACHE => Ok(Vec::new()),
                TEST_UNIT_READY => {
                    let dev_lock = self.dev.lock().unwrap();
                    if dev_lock.disk_image.is_none() {
                        Err(anyhow!("No scsi backend!"))
                    } else {
                        Ok(Vec::new())
                    }
                }
                INQUIRY => scsi_command_emulate_inquiry(&self.cmd, &self.dev),
                READ_CAPACITY_10 => scsi_command_emulate_read_capacity_10(&self.cmd, &self.dev),
                MODE_SENSE | MODE_SENSE_10 => scsi_command_emulate_mode_sense(&self.cmd, &self.dev),
                SERVICE_ACTION_IN_16 => {
                    scsi_command_emulate_service_action_in_16(&self.cmd, &self.dev)
                }
                _ => {
                    not_supported_flag = true;
                    Err(anyhow!("Emulation scsi command is not supported now!"))
                }
            };
        }

        match result {
            Ok(outbuf) => {
                self.cmd_complete(
                    &iocompletecb.mem_space,
                    VIRTIO_SCSI_S_OK,
                    GOOD,
                    sense,
                    &outbuf,
                )?;
            }
            Err(ref e) => {
                if not_supported_flag {
                    info!(
                        "emulation scsi command {:#x} is no supported",
                        self.cmd.command
                    );
                    self.cmd_complete(
                        &iocompletecb.mem_space,
                        VIRTIO_SCSI_S_OK,
                        CHECK_CONDITION,
                        Some(SCSI_SENSE_INVALID_OPCODE),
                        &Vec::new(),
                    )?;
                } else {
                    error!(
                        "Error in processing scsi command 0x{:#x}, err is {:?}",
                        self.cmd.command, e
                    );
                    self.cmd_complete(
                        &iocompletecb.mem_space,
                        VIRTIO_SCSI_S_OK,
                        CHECK_CONDITION,
                        Some(SCSI_SENSE_INVALID_FIELD),
                        &Vec::new(),
                    )?;
                }
            }
        }

        Ok(())
    }

    fn set_scsi_sense(&self, sense: ScsiSense) {
        let mut req = self.virtioscsireq.lock().unwrap();
        // Response code: current errors(0x70).
        req.resp.sense[0] = 0x70;
        req.resp.sense[2] = sense.key;
        // Additional sense length: sense len - 8.
        req.resp.sense[7] = SCSI_SENSE_LEN as u8 - 8;
        req.resp.sense[12] = sense.asc;
        req.resp.sense[13] = sense.ascq;
        req.resp.sense_len = SCSI_SENSE_LEN;
    }

    fn cmd_complete(
        &self,
        mem_space: &Arc<AddressSpace>,
        response: u8,
        status: u8,
        scsisense: Option<ScsiSense>,
        outbuf: &[u8],
    ) -> Result<()> {
        if let Some(sense) = scsisense {
            self.set_scsi_sense(sense);
        }
        let mut req = self.virtioscsireq.lock().unwrap();
        req.resp.response = response;
        req.resp.status = status;
        req.resp.resid = 0;

        if !outbuf.is_empty() {
            for (idx, iov) in req.iovec.iter().enumerate() {
                if outbuf.len() as u64 > iov.iov_len as u64 {
                    debug!(
                        "cmd is {:x}, outbuf len is {}, iov_len is {}, idx is {}, iovec size is {}",
                        self.cmd.command,
                        outbuf.len(),
                        iov.iov_len,
                        idx,
                        req.iovec.len()
                    );
                }

                write_buf_mem(outbuf, iov.iov_len, iov.iov_base)
                    .with_context(|| "Failed to write buf for virtio scsi iov")?;
            }
        }

        req.complete(mem_space);
        Ok(())
    }
}

fn write_buf_mem(buf: &[u8], max: u64, hva: u64) -> Result<()> {
    let mut slice = unsafe {
        std::slice::from_raw_parts_mut(hva as *mut u8, cmp::min(buf.len(), max as usize))
    };
    (&mut slice)
        .write(buf)
        .with_context(|| format!("Failed to write buf(hva:{})", hva))?;

    Ok(())
}

pub const EMULATE_SCSI_OPS: u32 = 0;
pub const DMA_SCSI_OPS: u32 = 1;

fn scsi_operation_type(op: u8) -> u32 {
    match op {
        READ_6 | READ_10 | READ_12 | READ_16 | WRITE_6 | WRITE_10 | WRITE_12 | WRITE_16
        | WRITE_VERIFY_10 | WRITE_VERIFY_12 | WRITE_VERIFY_16 => DMA_SCSI_OPS,
        _ => EMULATE_SCSI_OPS,
    }
}

//   lun: [u8, 8]
//   | Byte 0 | Byte 1 | Byte 2 | Byte 3 | Byte 4 | Byte 5 | Byte 6 | Byte 7 |
//   |    1   | target |       lun       |                 0                 |
pub fn virtio_scsi_get_lun(lun: [u8; 8]) -> u16 {
    (((lun[2] as u16) << 8) | (lun[3] as u16)) & 0x3FFF
}

fn scsi_cdb_length(cdb: &[u8; VIRTIO_SCSI_CDB_DEFAULT_SIZE]) -> i32 {
    match cdb[0] >> 5 {
        // CDB[0]: Operation Code Byte. Bits[0-4]: Command Code. Bits[5-7]: Group Code.
        // Group Code |  Meaning            |
        // 000b       |  6 bytes commands.  |
        // 001b       |  10 bytes commands. |
        // 010b       |  10 bytes commands. |
        // 011b       |  reserved.          |
        // 100b       |  16 bytes commands. |
        // 101b       |  12 bytes commands. |
        // 110b       |  vendor specific.   |
        // 111b       |  vendor specific.   |
        0 => 6,
        1 | 2 => 10,
        4 => 16,
        5 => 12,
        _ => -1,
    }
}

fn scsi_cdb_xfer(cdb: &[u8; VIRTIO_SCSI_CDB_DEFAULT_SIZE]) -> i32 {
    let mut xfer = match cdb[0] >> 5 {
        // Group Code  |  Transfer length. |
        // 000b        |  Byte[4].         |
        // 001b        |  Bytes[7-8].      |
        // 010b        |  Bytes[7-8].      |
        // 100b        |  Bytes[10-13].    |
        // 101b        |  Bytes[6-9].      |
        0 => cdb[4] as i32,
        1 | 2 => BigEndian::read_u16(&cdb[7..]) as i32,
        4 => BigEndian::read_u32(&cdb[10..]) as i32,
        5 => BigEndian::read_u32(&cdb[6..]) as i32,
        _ => -1,
    };

    match cdb[0] {
        TEST_UNIT_READY | REWIND | START_STOP | SET_CAPACITY | WRITE_FILEMARKS
        | WRITE_FILEMARKS_16 | SPACE | RESERVE | RELEASE | ERASE | ALLOW_MEDIUM_REMOVAL
        | SEEK_10 | SYNCHRONIZE_CACHE | SYNCHRONIZE_CACHE_16 | LOCATE_16 | LOCK_UNLOCK_CACHE
        | SET_CD_SPEED | SET_LIMITS | WRITE_LONG_10 | UPDATE_BLOCK | RESERVE_TRACK
        | SET_READ_AHEAD | PRE_FETCH | PRE_FETCH_16 | ALLOW_OVERWRITE => {
            xfer = 0;
        }
        VERIFY_10 | VERIFY_12 | VERIFY_16 => {
            if cdb[1] & 2 == 0 {
                xfer = 0;
            } else if cdb[1] & 4 != 0 {
                xfer = 1;
            }
            xfer *= SCSI_DEFAULT_BLOCK_SIZE;
        }
        WRITE_SAME_10 | WRITE_SAME_16 => {
            if cdb[1] & 1 != 0 {
                xfer = 0;
            } else {
                xfer = SCSI_DEFAULT_BLOCK_SIZE;
            }
        }
        READ_CAPACITY_10 => {
            xfer = 8;
        }
        READ_BLOCK_LIMITS => {
            xfer = 6;
        }
        SEND_VOLUME_TAG => {
            xfer = i32::from(cdb[9]) | i32::from(cdb[8]) << 8;
        }
        WRITE_6 | READ_6 | READ_REVERSE => {
            // length 0 means 256 blocks.
            if xfer == 0 {
                xfer = 256 * SCSI_DEFAULT_BLOCK_SIZE;
            }
        }
        WRITE_10 | WRITE_VERIFY_10 | WRITE_12 | WRITE_VERIFY_12 | WRITE_16 | WRITE_VERIFY_16
        | READ_10 | READ_12 | READ_16 => {
            xfer *= SCSI_DEFAULT_BLOCK_SIZE;
        }
        FORMAT_UNIT => {
            xfer = match cdb[1] & 16 {
                0 => 0,
                _ => match cdb[1] & 32 {
                    0 => 4,
                    _ => 8,
                },
            };
        }
        INQUIRY | RECEIVE_DIAGNOSTIC | SEND_DIAGNOSTIC => {
            xfer = i32::from(cdb[4]) | i32::from(cdb[3]) << 8;
        }
        READ_CD | READ_BUFFER | WRITE_BUFFER | SEND_CUE_SHEET => {
            xfer = i32::from(cdb[8]) | i32::from(cdb[7]) << 8 | (u32::from(cdb[6]) << 16) as i32;
        }
        PERSISTENT_RESERVE_OUT => {
            xfer = BigEndian::read_i32(&cdb[5..]);
        }
        ERASE_12 | MECHANISM_STATUS | READ_DVD_STRUCTURE | SEND_DVD_STRUCTURE | MAINTENANCE_OUT
        | MAINTENANCE_IN => {}
        ATA_PASSTHROUGH_12 => {}
        ATA_PASSTHROUGH_16 => {}
        _ => {}
    }
    xfer
}

fn scsi_cdb_lba(cdb: &[u8; VIRTIO_SCSI_CDB_DEFAULT_SIZE]) -> i64 {
    match cdb[0] >> 5 {
        // Group Code  |  Logical Block Address.       |
        // 000b        |  Byte[1].bits[0-4]~Byte[3].   |
        // 001b        |  Bytes[2-5].                  |
        // 010b        |  Bytes[2-5].                  |
        // 100b        |  Bytes[2-9].                  |
        // 101b        |  Bytes[2-5].                  |
        0 => (BigEndian::read_u32(&cdb[0..]) & 0x1fffff) as i64,
        1 | 2 | 5 => BigEndian::read_u32(&cdb[2..]) as i64,
        4 => BigEndian::read_u64(&cdb[2..]) as i64,
        _ => -1,
    }
}

fn scsi_cdb_xfer_mode(cdb: &[u8; VIRTIO_SCSI_CDB_DEFAULT_SIZE]) -> ScsiXferMode {
    match cdb[0] {
        WRITE_6
        | WRITE_10
        | WRITE_VERIFY_10
        | WRITE_12
        | WRITE_VERIFY_12
        | WRITE_16
        | WRITE_VERIFY_16
        | VERIFY_10
        | VERIFY_12
        | VERIFY_16
        | COPY
        | COPY_VERIFY
        | COMPARE
        | CHANGE_DEFINITION
        | LOG_SELECT
        | MODE_SELECT
        | MODE_SELECT_10
        | SEND_DIAGNOSTIC
        | WRITE_BUFFER
        | FORMAT_UNIT
        | REASSIGN_BLOCKS
        | SEARCH_EQUAL
        | SEARCH_HIGH
        | SEARCH_LOW
        | UPDATE_BLOCK
        | WRITE_LONG_10
        | WRITE_SAME_10
        | WRITE_SAME_16
        | UNMAP
        | SEARCH_HIGH_12
        | SEARCH_EQUAL_12
        | SEARCH_LOW_12
        | MEDIUM_SCAN
        | SEND_VOLUME_TAG
        | SEND_CUE_SHEET
        | SEND_DVD_STRUCTURE
        | PERSISTENT_RESERVE_OUT
        | MAINTENANCE_OUT
        | SET_WINDOW
        | SCAN => ScsiXferMode::ScsiXferToDev,

        ATA_PASSTHROUGH_12 | ATA_PASSTHROUGH_16 => match cdb[2] & 0x8 {
            0 => ScsiXferMode::ScsiXferToDev,
            _ => ScsiXferMode::ScsiXferFromDev,
        },

        _ => ScsiXferMode::ScsiXferFromDev,
    }
}

/// VPD: Virtual Product Data.
fn scsi_command_emulate_vpd_page(
    cmd: &ScsiCommand,
    dev: &Arc<Mutex<ScsiDevice>>,
) -> Result<Vec<u8>> {
    let buflen: usize;
    let mut outbuf: Vec<u8> = vec![0; 4];

    let dev_lock = dev.lock().unwrap();
    let page_code = cmd.buf[2];

    outbuf[0] = dev_lock.scsi_type as u8 & 0x1f;
    outbuf[1] = page_code;

    match page_code {
        0x00 => {
            // Supported VPD Pages.
            outbuf.push(0_u8);
            if !dev_lock.state.serial.is_empty() {
                // 0x80: Unit Serial Number.
                outbuf.push(0x80);
            }
            // 0x83: Device Identification.
            outbuf.push(0x83);
            if dev_lock.scsi_type == SCSI_TYPE_DISK {
                // 0xb0: Block Limits.
                outbuf.push(0xb0);
                // 0xb1: Block Device Characteristics.
                outbuf.push(0xb1);
                // 0xb2: Logical Block Provisioning.
                outbuf.push(0xb2);
            }
            buflen = outbuf.len();
        }
        0x80 => {
            // Unit Serial Number.
            let len = dev_lock.state.serial.len();
            if len == 0 {
                bail!("Missed serial number!");
            }

            let l = cmp::min(SCSI_INQUIRY_VPD_SERIAL_NUMBER_MAX_LEN, len);
            let mut serial_vec = dev_lock.state.serial.as_bytes().to_vec();
            serial_vec.truncate(l);
            outbuf.append(&mut serial_vec);
            buflen = outbuf.len();
        }
        0x83 => {
            // Device Identification.
            let mut len: u8 = dev_lock.state.device_id.len() as u8;
            if len > (255 - 8) {
                len = 255 - 8;
            }

            if len > 0 {
                // 0x2: Code Set: ASCII, Protocol Identifier: FCP-4.
                // 0: Identifier Type, Association, Reserved, Piv.
                // 0: Reserved.
                // len: identifier length.
                outbuf.append(&mut [0x2_u8, 0_u8, 0_u8, len].to_vec());

                let mut device_id_vec = dev_lock.state.device_id.as_bytes().to_vec();
                device_id_vec.truncate(len as usize);
                outbuf.append(&mut device_id_vec);
            }
            buflen = outbuf.len();
        }
        0xb0 => {
            // Block Limits.
            if dev_lock.scsi_type == SCSI_TYPE_ROM {
                bail!("Invalid scsi type: SCSI_TYPE_ROM !");
            }
            outbuf.resize(64, 0);

            // Byte[4]: bit 0: wsnz: Write Same Non-zero.
            // Byte[5] = 0: Maximum compare and write length (COMPARE_AND_WRITE not supported).
            // Byte[6-7] = 0: Optimal transfer length granularity.
            // Byte[8-11]: Maximum transfer length.
            // Byte[12-15] = 0: Optimal Transfer Length.
            // Byte[16-19] = 0: Maxium Prefetch Length.
            // Byte[20-23]: Maximum unmap lba count.
            // Byte[24-27]: Maximum unmap block descriptor count.
            // Byte[28-31]: Optimal unmap granulatity.
            // Byte[32-35] = 0: Unmap Granularity alignment.
            // Byte[36-43]: Maximum write same length.
            // Bytes[44-47] = 0: Maximum atomic Transfer length.
            // Bytes[48-51] = 0: Atomic Alignment.
            // Bytes[52-55] = 0: Atomic Transfer Length Granularity.
            // Bytes[56-59] = 0: Maximum Atomic Transfer Length With Atomic Boundary.
            // Bytes[60-63] = 0: Maximum Atomic Boundary Size.
            outbuf[4] = 1;
            let max_xfer_length: u32 = u32::MAX / 512;
            BigEndian::write_u32(&mut outbuf[8..12], max_xfer_length);
            let max_unmap_sectors: u32 = (1_u32 << 30) / 512;
            BigEndian::write_u32(&mut outbuf[20..24], max_unmap_sectors);
            let max_unmap_block_desc: u32 = 255;
            BigEndian::write_u32(&mut outbuf[24..28], max_unmap_block_desc);
            let opt_unmap_granulatity: u32 = (1_u32 << 12) / 512;
            BigEndian::write_u32(&mut outbuf[28..32], opt_unmap_granulatity);
            BigEndian::write_u64(&mut outbuf[36..44], max_xfer_length as u64);
            buflen = outbuf.len();
        }
        0xb1 => {
            // Block Device Characteristics.
            // 0: Medium Rotation Rate: 2Bytes.
            // 0: Medium Rotation Rate: 2Bytes.
            // 0: Product Type.
            // 0: Nominal Form Factor, Wacereq, Wabereq.
            // 0: Vbuls, Fuab, Bocs, Reserved, Zoned, Reserved.
            outbuf.append(&mut [0_u8, 0_u8, 0_u8, 0_u8, 0_u8].to_vec());
            buflen = 0x40;
        }
        0xb2 => {
            // Logical Block Provisioning.
            // 0: Threshold exponent.
            // 0xe0: LBPU | LBPWS | LBPWS10 | LBPRZ | ANC_SUP | DP.
            // 0: Threshold percentage | Provisioning Type.
            // 0: Threshold percentage.
            outbuf.append(&mut [0_u8, 0xe0_u8, 1_u8, 0_u8].to_vec());
            buflen = 8;
        }
        _ => {
            bail!("Invalid INQUIRY pagecode {}", page_code);
        }
    }

    // It's OK for just using outbuf bit 3, because all page_code's buflen in stratovirt is less than 255 now.
    outbuf[3] = buflen as u8 - 4;
    Ok(outbuf)
}

fn scsi_command_emulate_target_inquiry(lun: u16, cmd: &ScsiCommand) -> Result<Vec<u8>> {
    let mut outbuf: Vec<u8> = vec![0; 4];

    // Byte1: bit0: EVPD (Enable Vital product bit).
    if cmd.buf[1] == 0x1 {
        // Vital Product Data.
        // Byte2: Page Code.
        let page_code = cmd.buf[2];
        outbuf[1] = page_code;
        match page_code {
            0x00 => {
                // Supported page codes.
                // Page Length: outbuf.len() - 4. Supported VPD page list only has 0x00 item.
                outbuf[3] = 0x1;
                // Supported VPD page list. Only support this page.
                outbuf.push(0x00);
            }
            _ => {
                bail!("Emulate target inquiry invalid page code {:x}", page_code);
            }
        }
        return Ok(outbuf);
    }

    // EVPD = 0 means it's a Standard INQUIRY command.
    // Byte2: page code.
    if cmd.buf[2] != 0 {
        bail!("Invalid standatd inquiry command!");
    }

    outbuf.resize(SCSI_TARGET_INQUIRY_LEN as usize, 0);
    let len = cmp::min(cmd.xfer, SCSI_TARGET_INQUIRY_LEN);

    // outbuf.
    // Byte0: Peripheral Qualifier | peripheral device type.
    // Byte1：RMB.
    // Byte2: VERSION.
    // Byte3: NORMACA | HISUP | Response Data Format.
    // Byte4: Additional length(outbuf.len() - 5).
    // Byte5: SCCS | ACC | TPGS | 3PC | RESERVED | PROTECT.
    // Byte6: ENCSERV | VS | MULTIP | ADDR16.
    // Byte7: WBUS16 | SYNC | CMDQUE | VS.
    if lun != 0 {
        outbuf[0] = TYPE_NO_LUN;
    } else {
        outbuf[0] = TYPE_UNKNOWN | TYPE_INACTIVE;
        // scsi version.
        outbuf[2] = 5;
        // HISUP(hierarchical support). Response Data Format(must be 2).
        outbuf[3] = 0x12;
        outbuf[4] = len as u8 - 5;
        // SYNC, CMDQUE(the logical unit supports the task management model).
        outbuf[7] = 0x12;
    }

    Ok(outbuf)
}

fn scsi_command_emulate_inquiry(
    cmd: &ScsiCommand,
    dev: &Arc<Mutex<ScsiDevice>>,
) -> Result<Vec<u8>> {
    // Vital product data.
    if cmd.buf[1] == 0x1 {
        return scsi_command_emulate_vpd_page(cmd, dev);
    }

    if cmd.buf[2] != 0 {
        bail!("Invalid INQUIRY!");
    }

    let buflen = cmp::min(cmd.xfer, SCSI_MAX_INQUIRY_LEN);
    let mut outbuf: Vec<u8> = vec![0; buflen as usize];

    let dev_lock = dev.lock().unwrap();

    outbuf[0] = (dev_lock.scsi_type & 0x1f) as u8;
    outbuf[1] = match dev_lock.state.features & SCSI_DISK_F_REMOVABLE {
        1 => 0x80,
        _ => 0,
    };

    let product_bytes = dev_lock.state.product.as_bytes();
    let product_len = cmp::min(product_bytes.len(), SCSI_INQUIRY_PRODUCT_MAX_LEN);
    let vendor_bytes = dev_lock.state.vendor.as_bytes();
    let vendor_len = cmp::min(vendor_bytes.len(), SCSI_INQUIRY_VENDOR_MAX_LEN);
    let version_bytes = dev_lock.state.version.as_bytes();
    let vension_len = cmp::min(version_bytes.len(), SCSI_INQUIRY_VERSION_MAX_LEN);

    outbuf[16..16 + product_len].copy_from_slice(product_bytes);
    outbuf[8..8 + vendor_len].copy_from_slice(vendor_bytes);
    outbuf[32..32 + vension_len].copy_from_slice(version_bytes);

    drop(dev_lock);

    // scsi version: 5.
    outbuf[2] = 5;
    outbuf[3] = (2 | 0x10) as u8;

    if buflen > 36 {
        outbuf[4] = (buflen - 5) as u8;
    } else {
        outbuf[4] = 36 - 5;
    }

    // TCQ.
    outbuf[7] = 0x12;

    Ok(outbuf)
}

fn scsi_command_emulate_read_capacity_10(
    cmd: &ScsiCommand,
    dev: &Arc<Mutex<ScsiDevice>>,
) -> Result<Vec<u8>> {
    if cmd.buf[8] & 1 == 0 && cmd.lba != 0 {
        // PMI(Partial Medium Indicator)
        bail!("Invalid scsi cmd READ_CAPACITY_10!");
    }

    let dev_lock = dev.lock().unwrap();
    let mut outbuf: Vec<u8> = vec![0; 8];
    let nb_sectors = cmp::min(dev_lock.disk_sectors as u32, u32::MAX);

    // Bytes[0-3]: Returned Logical Block Address.
    // Bytes[4-7]: Logical Block Length In Bytes.
    BigEndian::write_u32(&mut outbuf[0..4], nb_sectors);
    BigEndian::write_u32(&mut outbuf[4..8], DEFAULT_SECTOR_SIZE);

    Ok(outbuf)
}

fn scsi_command_emulate_mode_sense(
    cmd: &ScsiCommand,
    dev: &Arc<Mutex<ScsiDevice>>,
) -> Result<Vec<u8>> {
    // disable block descriptors(DBD) bit.
    let mut dbd: bool = cmd.buf[1] & 0x8 != 0;
    let page_code = cmd.buf[2] & 0x3f;
    let page_control = (cmd.buf[2] & 0xc0) >> 6;
    let mut outbuf: Vec<u8> = vec![0];
    let dev_lock = dev.lock().unwrap();
    let mut dev_specific_parameter: u8 = 0;
    let nb_sectors = dev_lock.disk_sectors;

    debug!(
        "MODE SENSE page_code {:x}, page_control {:x}, subpage {:x}, dbd bit {:x}, Allocation length {}",
        page_code,
        page_control,
        cmd.buf[3],
        cmd.buf[1] & 0x8,
        cmd.buf[4]
    );

    if dev_lock.scsi_type == SCSI_TYPE_DISK {
        if dev_lock.state.features & (1 << SCSI_DISK_F_DPOFUA) != 0 {
            dev_specific_parameter = 0x10;
        }
    } else {
        dbd = true;
    }
    drop(dev_lock);

    if cmd.command == MODE_SENSE {
        outbuf.resize(4, 0);
        // Device Specific Parameter.
        outbuf[2] = dev_specific_parameter;
    } else {
        // MODE_SENSE_10.
        outbuf.resize(8, 0);
        // Device Specific Parameter.
        outbuf[3] = dev_specific_parameter;
    }

    if !dbd && nb_sectors > 0 {
        if cmd.command == MODE_SENSE {
            // Block Descriptor Length.
            outbuf[3] = 8;
        } else {
            // Block Descriptor Length.
            outbuf[7] = 8;
        }

        // Block descriptors.
        // Byte[0]: density code.
        // Bytes[1-3]: number of blocks.
        // Byte[4]: Reserved.
        // Byte[5-7]: Block Length.
        let mut block_desc: Vec<u8> = vec![0; 8];
        BigEndian::write_u32(&mut block_desc[0..4], nb_sectors as u32 & 0xffffff);
        BigEndian::write_u32(&mut block_desc[4..8], DEFAULT_SECTOR_SIZE);
        outbuf.append(&mut block_desc);
    }

    if page_control == 3 {
        bail!("Invalid Mode Sense command, Page control 0x11 is not supported!");
    }

    if page_code == 0x3f {
        // 3Fh Return all pages not including subpages.
        for pg in 0..page_code {
            let _ = scsi_command_emulate_mode_sense_page(pg, page_control, &mut outbuf);
        }
    } else {
        scsi_command_emulate_mode_sense_page(page_code, page_control, &mut outbuf)?;
    }

    // The Mode Data Length field indicates the length in bytes of the following data
    // that is available to be transferred. The Mode data length does not include the
    // number of bytes in the Mode Data Length field.
    let buflen = outbuf.len();
    if cmd.command == MODE_SENSE {
        outbuf[0] = (buflen - 1) as u8;
    } else {
        outbuf[0] = (((buflen - 2) >> 8) & 0xff) as u8;
        outbuf[1] = ((buflen - 2) & 0xff) as u8;
    }

    Ok(outbuf)
}

fn scsi_command_emulate_mode_sense_page(
    page: u8,
    page_control: u8,
    outbuf: &mut Vec<u8>,
) -> Result<Vec<u8>> {
    let buflen = outbuf.len();
    match page {
        MODE_PAGE_CACHING => {
            // Caching Mode Page.
            outbuf.resize(buflen + 20, 0);
            outbuf[buflen] = page;
            outbuf[buflen + 1] = 18;
            // 0x4: WCE(Write Cache Enable).
            outbuf[buflen + 2] = 0x4;
        }
        MODE_PAGE_R_W_ERROR => {
            // Read-Write Error Recovery mode page.
            outbuf.resize(buflen + 12, 0);
            outbuf[buflen] = page;
            outbuf[buflen + 1] = 10;

            if page_control != 1 {
                // 0x80: AWRE(Automatic Write Reallocation Enabled).
                outbuf[buflen + 2] = 0x80;
            }
        }
        _ => {
            bail!(
                "Invalid Mode Sense command, page control ({:x}), page ({:x})",
                page_control,
                page
            );
        }
    }

    Ok(outbuf.to_vec())
}

fn scsi_command_emulate_report_luns(
    cmd: &ScsiCommand,
    dev: &Arc<Mutex<ScsiDevice>>,
) -> Result<Vec<u8>> {
    let dev_lock = dev.lock().unwrap();
    // Byte 0-3: Lun List Length. Byte 4-7: Reserved.
    let mut outbuf: Vec<u8> = vec![0; 8];
    let target = dev_lock.config.target;

    if cmd.xfer < 16 {
        bail!("scsi REPORT LUNS xfer {} too short!", cmd.xfer);
    }

    //Byte2: SELECT REPORT:00h/01h/02h. 03h to FFh is reserved.
    if cmd.buf[2] > 2 {
        bail!(
            "Invalid REPORT LUNS cmd, SELECT REPORT Byte is {}",
            cmd.buf[2]
        );
    }

    let scsi_bus = dev_lock.parent_bus.upgrade().unwrap();
    let scsi_bus_clone = scsi_bus.lock().unwrap();

    drop(dev_lock);

    for (_pos, device) in scsi_bus_clone.devices.iter() {
        let device_lock = device.lock().unwrap();
        if device_lock.config.target != target {
            drop(device_lock);
            continue;
        }
        let len = outbuf.len();
        if device_lock.config.lun < 256 {
            outbuf.push(0);
            outbuf.push(device_lock.config.lun as u8);
        } else {
            outbuf.push(0x40 | ((device_lock.config.lun >> 8) & 0xff) as u8);
            outbuf.push((device_lock.config.lun & 0xff) as u8);
        }
        outbuf.resize(len + 8, 0);
        drop(device_lock);
    }

    let len: u32 = outbuf.len() as u32 - 8;
    BigEndian::write_u32(&mut outbuf[0..4], len);
    Ok(outbuf)
}

fn scsi_command_emulate_service_action_in_16(
    cmd: &ScsiCommand,
    dev: &Arc<Mutex<ScsiDevice>>,
) -> Result<Vec<u8>> {
    // Read Capacity(16) Command.
    // Byte 0: Operation Code(0x9e)
    // Byte 1: bit0 - bit4: Service Action(0x10), bit 5 - bit 7: Reserved.
    if cmd.buf[1] & 0x1f == SUBCODE_READ_CAPACITY_16 {
        let dev_lock = dev.lock().unwrap();
        let mut outbuf: Vec<u8> = vec![0; 32];
        let nb_sectors = dev_lock.disk_sectors;

        drop(dev_lock);

        // Byte[0-7]: Returned Logical BLock Address.
        // Byte[8-11]: Logical Block Length in Bytes.
        BigEndian::write_u64(&mut outbuf[0..8], nb_sectors);
        BigEndian::write_u32(&mut outbuf[8..12], DEFAULT_SECTOR_SIZE);

        return Ok(outbuf);
    }

    bail!(
        "Invalid combination Scsi Command, operation code ({:x}), service action ({:x})",
        SERVICE_ACTION_IN_16,
        cmd.buf[1] & 31
    );
}
