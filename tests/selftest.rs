//! Runs the same end-to-end flow as `VeilForge.exe --selftest`: every mode,
//! the stand-in shortcut flow through the command-line code path, recovery
//! copies and panic restore, all inside one temp folder that is removed again.

#[test]
fn every_mode_end_to_end_in_a_temp_folder() {
    match veilforge::selftest::run() {
        Ok(log) => {
            for line in &log {
                println!("{line}");
            }
            assert!(log.len() >= 7, "every stage ran: {log:#?}");
        }
        Err(e) => panic!("{e}"),
    }
}
