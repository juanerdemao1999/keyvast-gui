use kv_rhd::{
    AuxCommandSlot, BoardPort, RHD_COMMAND_LIST_LEN, Rhd2000CommandType, Rhd2000Registers,
    create_rhd2000_command,
};

#[test]
fn encodes_rhd2000_spi_commands_like_intan() {
    assert_eq!(
        create_rhd2000_command(Rhd2000CommandType::Convert, Some(12), None)
            .expect("valid convert"),
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
        create_rhd2000_command(Rhd2000CommandType::Calibrate, None, None)
            .expect("valid calibrate"),
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

    assert_eq!(
        values,
        vec![
            222, 66, 4, 2, 156, 64, 128, 0, 17, 128, 16, 128, 44, 134, 255, 255, 255, 255, 255,
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
