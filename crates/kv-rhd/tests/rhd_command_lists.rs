// `from_register63` literals are grouped as `2-bit chip field | 6-bit revision`
// for readability, which deliberately uses uneven byte groups.
#![allow(clippy::unusual_byte_groupings)]

use kv_rhd::{
    AuxCommandSlot, BoardPort, RHD_COMMAND_LIST_LEN, Rhd2000CommandType, Rhd2000Registers,
    RhdChipType, create_rhd2000_command,
};

#[test]
fn encodes_rhd2000_spi_commands_like_intan() {
    assert_eq!(
        create_rhd2000_command(Rhd2000CommandType::Convert, Some(12), None).expect("valid convert"),
        0x0c00
    );
    assert_eq!(
        create_rhd2000_command(Rhd2000CommandType::RegisterRead, Some(63), None)
            .expect("valid read"),
        0xff00
    );
    assert_eq!(
        create_rhd2000_command(Rhd2000CommandType::RegisterWrite, Some(4), Some(156))
            .expect("valid write"),
        0x849c
    );
    assert_eq!(
        create_rhd2000_command(Rhd2000CommandType::Calibrate, None, None).expect("valid calibrate"),
        0x5500
    );
    assert_eq!(
        create_rhd2000_command(Rhd2000CommandType::ClearCalibration, None, None)
            .expect("valid clear"),
        0x6a00
    );
}

#[test]
fn default_registers_match_open_ephys_rhd_30khz_settings() {
    let registers = Rhd2000Registers::open_ephys_default();
    let values = (0_u8..=21)
        .map(|register| {
            registers
                .register_value(register)
                .expect("register should exist")
        })
        .collect::<Vec<_>>();

    // Golden register set matching the Open Ephys RHD plugin reference recording
    // (Record Node settings.xml). Each field-specific byte is cross-checked against
    // OE's documented settings, not just against current code output:
    //   - Reg 4 = 140: DSP offset-removal OFF (dsp_en bit clear) to match OE
    //     DSPOffset="0". Byte = weak_miso(0x80) | dsp_cutoff_freq(12); the cutoff
    //     value is retained-but-inert, exactly as OE carries a DSPCutoffFreq with
    //     DSPOffset=0. (Was 156 when DSP was erroneously enabled.)
    //   - Regs 8,10 = 22,23: upper-bandwidth corner. OE uses HighCut=7500 Hz, which
    //     the Intan resistor fit maps to rh1_dac1=22, rh2_dac1=23. (A 10 kHz corner
    //     would give 17/16.)
    //   - Regs 12,13 = 127,255: lower-bandwidth corner. OE uses LowCut=0.0955 Hz.
    //     Below 0.15 Hz the Intan fit engages the 3 MOhm resistor and saturates the
    //     RL DACs: rl_dac1=127, rl_dac2=63, rl_dac3=1 (reg13 also carries
    //     adc_aux3_en in bit 7 -> 128|64|63 = 255). (Was 44,134 at LowCut=1.0 Hz.)
    assert_eq!(
        values,
        vec![
            222, 66, 4, 2, 140, 64, 128, 0, 22, 128, 23, 128, 127, 255, 255, 255, 255, 255, 255,
            255, 255, 255,
        ]
    );
}

#[test]
fn register_config_lists_are_128_commands_and_calibrate_only_bank0() {
    let registers = Rhd2000Registers::open_ephys_default();

    let calibrating = registers
        .create_command_list_register_config(true)
        .expect("calibrating list");
    let normal = registers
        .create_command_list_register_config(false)
        .expect("normal list");

    assert_eq!(calibrating.len(), RHD_COMMAND_LIST_LEN);
    assert_eq!(normal.len(), RHD_COMMAND_LIST_LEN);
    assert_eq!(calibrating[0], 0xff00);
    assert_eq!(calibrating[2], 0x80de);
    assert_eq!(calibrating[54], 0x5500);
    assert_eq!(normal[54], 0xff00);
    assert_eq!(normal[55], 0x92ff);
    assert_eq!(normal[58], 0x95ff);
}

#[test]
fn aux_command_lists_are_128_commands() {
    let mut registers = Rhd2000Registers::open_ephys_default();
    registers.set_dig_out_low();

    let dig_out = registers
        .create_command_list_update_dig_out()
        .expect("dig out list");
    let temp_sensor = registers
        .create_command_list_temp_sensor()
        .expect("temp sensor list");

    assert_eq!(dig_out.len(), RHD_COMMAND_LIST_LEN);
    assert_eq!(temp_sensor.len(), RHD_COMMAND_LIST_LEN);
    assert_eq!(dig_out[0], 0x8304);
    assert_eq!(temp_sensor[0], 0x2000);
    assert_eq!(temp_sensor[1], 0x2100);
    assert_eq!(temp_sensor[2], 0x2200);
}

#[test]
fn board_port_bit_shifts_match_frontpanel_bank_wires() {
    assert_eq!(BoardPort::PortA.bit_shift(), 0);
    assert_eq!(BoardPort::PortH.bit_shift(), 28);
    assert_eq!(BoardPort::all().len(), 8);
    assert_eq!(format!("{:?}", AuxCommandSlot::AuxCmd3), "AuxCmd3");
}

// ---------- H20: RhdChipType dispatch tests ----------

#[test]
fn rhd_chip_type_from_register63_known_values() {
    // RHD2000 ROM register 63 holds the Intan chip ID as a literal value
    // (matching Open Ephys getDeviceId): 1 = RHD2132 (32ch), 2 = RHD2216 (16ch),
    // 4 = RHD2164 (64ch).
    let rhd2132 = RhdChipType::from_register63(1).unwrap();
    assert_eq!(rhd2132, RhdChipType::Rhd2132);
    assert_eq!(rhd2132.channel_count(), 32);
    assert_eq!(rhd2132.streams_per_headstage(), 1);

    let rhd2216 = RhdChipType::from_register63(2).unwrap();
    assert_eq!(rhd2216, RhdChipType::Rhd2216);
    assert_eq!(rhd2216.channel_count(), 16);
    assert_eq!(rhd2216.streams_per_headstage(), 1);

    let rhd2164 = RhdChipType::from_register63(4).unwrap();
    assert_eq!(rhd2164, RhdChipType::Rhd2164);
    assert_eq!(rhd2164.channel_count(), 64);
    assert_eq!(rhd2164.streams_per_headstage(), 2);
}

#[test]
fn rhd_chip_type_from_register63_ignores_high_byte() {
    // Only the low byte carries the chip ID; any upper-byte status bits are masked.
    let with_high = RhdChipType::from_register63(0xff00 | 1).unwrap();
    assert_eq!(with_high, RhdChipType::Rhd2132);
}

#[test]
fn rhd_chip_type_from_register63_unknown_returns_none() {
    // 3 is not a known Intan chip ID.
    assert!(RhdChipType::from_register63(3).is_none());
    assert!(RhdChipType::from_register63(0).is_none());
}
