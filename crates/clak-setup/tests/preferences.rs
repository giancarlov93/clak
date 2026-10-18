mod common;

use clak_setup::preferences::Preferences;
use common::Sandbox;

#[test]
fn connection_sound_is_opt_in_and_independent_of_other_preferences() {
    let sandbox = Sandbox::new();
    let home = sandbox.dir(&["hub"]);
    let preferences = Preferences::new(&home);
    assert!(!preferences.connection_sound());
    preferences.set_consent_sound(false).unwrap();
    preferences.set_notifications(false).unwrap();
    preferences.set_connection_sound(true).unwrap();
    let reloaded = Preferences::new(&home);
    assert!(reloaded.connection_sound());
    assert!(!reloaded.consent_sound());
    assert!(!reloaded.notifications());
    reloaded.set_connection_sound(false).unwrap();
    assert!(!Preferences::new(&home).connection_sound());
}

#[test]
fn the_update_check_is_on_until_turned_off() {
    let sandbox = Sandbox::new();
    let home = sandbox.dir(&["hub"]);
    assert!(Preferences::new(&home).update_check());
    Preferences::new(&home).set_update_check(false).unwrap();
    assert!(!Preferences::new(&home).update_check());
}

#[test]
fn automatic_updates_are_opt_in() {
    let sandbox = Sandbox::new();
    let home = sandbox.dir(&["hub"]);
    assert!(!Preferences::new(&home).auto_update());
    Preferences::new(&home).set_auto_update(true).unwrap();
    assert!(Preferences::new(&home).auto_update());
}
