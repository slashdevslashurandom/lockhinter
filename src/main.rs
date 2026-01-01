//! This crate provides a utility called "lockhinter" that can be used to properly set and clear
//! the LockedHint property for one's `systemd` / `logind` session.
//!
//! Full desktop environments, like GNOME, typically set this property whenever a session is locked
//! or unlocked to indicate to other software whether ot not the user is currently working. For
//! example, GNOME's usbguard integration will allow new USB devices to connect to the computer
//! only while the session is unlocked. Other programs, like KeePassXC, have an option to
//! automatically lock the database when the session is locked.
//!
//! If one is using a window manager or compositor that doesn't set this property, the session will
//! be always treated as if it is unlocked, but adding that functionality may require implementing
//! a kind of systemd/logind integration that the UI's developer may not like (say, if it is also
//! intended for use on systems that don't use systemd).
//!
//! This standalone tool solves that problem. When `lockhinter` is launched, it will start the
//! locker tool provided in the command-line arguments (`swaylock` is a good option) and set the
//! LockedHint property up until the locker exits. If the locker terminates abnormally, returning a
//! non-zero code or through a signal, the LockedHint property will NOT be released, ensuring that
//! accidental or intentional attempts to crash the locker will not lead the programs that check
//! the LockedHint to falsely think the session was unlocked. (Most Wayland lockers, including
//! `swaylock`, interact with the Wayland compositor to take control of the desktop and prevent
//! an unlock in case of a crash as well, for similar reasons.)
#[warn(missing_docs)]
extern crate getopts;
use std::process::Command;
use std::collections::HashMap;
use std::process::ExitCode;
use glib::prelude::*;
use glib::variant::Variant;
use std::sync::mpsc;
use getopts::Options;
use std::env;
use std::thread;
use thiserror::Error;

/// Structure used to pass information about the D-Bus connection from the bus watcher callbacks
/// to the work thread.
struct BusConnectionData {
    /// A handle for the connection or None
    connection: Option<gio::DBusConnection>,
    /// The owner's name or None
    owner: Option<String>,
}

/// Possible errors that the program may return
#[derive(Error,Debug)]
pub enum LockHinterError {
    /// The type returned from a D-Bus call doesn't match the expected type
    #[error("returned value is not of the correct type")]
    VariantTypeMismatchError(#[from] glib::variant::VariantTypeMismatchError),
    /// The dictionary returned from a D-Bus call doesn't contain the necessary entry
    #[error("value with key {0} is missing from a dictionary")]
    ValueMissingError(String),
    /// Another error passed directly from the GLib crate
    #[error("GLib error")]
    GLibError(#[from] glib::Error),
}

/// Print a short instruction on how to use the program
fn print_usage(program: &str, opts: Options) {
    let brief = format!("Usage: {} FILE [options] [--] program [args...]", program);
    print!("{}", opts.usage(&brief));
}

/// Ask D-Bus for the current session's object path
fn get_current_session_object_path(connection: &gio::DBusConnection, owner: &str) -> Result<String, LockHinterError> {

    let response = connection.call_sync(
        Some(owner),
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
        "GetSessionByPID",
        Some(&(std::process::id(),).to_variant()),
        Some(&glib::VariantType::new("(o)").unwrap()),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE
    )?;

    // we expect a tuple consisting of a single string-like item
    let response_contents = <(String,)>::from_variant(&response).unwrap();

    Ok(response_contents.0)

}

/// Obtain the session's state, based on the session object path. The Ok return is a tuple with two
/// items, a string describing the session state and a boolean containing the LockedHint value.
fn get_session_state(connection: &gio::DBusConnection, owner: &str, session_object_path: &str) -> Result<(String,bool),LockHinterError> {

    let response = connection.call_sync(
        Some(owner),
        session_object_path,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        Some(&("org.freedesktop.login1.Session",).to_variant()),
        Some(&glib::VariantType::new("(a{sv})").unwrap()), //tuple of an array of dictionary
                                                           //entries
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE
    )?;

    // get the hashmap corresponding to the dictentry array
    let properties: HashMap<String,Variant> = response.try_get::<(HashMap<String,Variant>,)>()?.0;

    let state: String = match properties.get("State") {
        Some(v) => v.try_get()?,
        None => { return Err(LockHinterError::ValueMissingError("State".to_string())); },
    };

    let locked_hint: bool = match properties.get("LockedHint") {
        Some(v) => v.try_get()?,
        None => { return Err(LockHinterError::ValueMissingError("LockedHint".to_string())); },
    };

    return Ok((state, locked_hint));

}

/// Set (or clear) the LockedHint property for the session specified.
fn set_locked_hint(connection: &gio::DBusConnection, owner: &str, session_object_path: &str, value: bool) -> Result<(),LockHinterError> {

    let _response = connection.call_sync(
        Some(owner),
        session_object_path,
        "org.freedesktop.login1.Session",
        "SetLockedHint",
        Some(&(value,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE
    )?;

    return Ok(());
}

fn main() -> ExitCode {

    let mut opts = Options::new();
    opts.optflag("c","check","do not run any locker, simply check whether LockedHint is set and output TRUE or FALSE");
    opts.optflag("f","force","do not exit if LockedHint already set, clear upon exit");
    opts.optflag("h","help","show usage");

    let args: Vec<String> = env::args().collect();

    // the first non-occupied parameter is the name of the program to call
    let program = args[0].clone(); 

    // the ones after that are the program's arguments
    let matches = match opts.parse(&args[1..]) {
        Ok(m) => {m}
        Err(f) => {panic!("{}", f.to_string())}
    };

    if matches.opt_present("h") {
        print_usage(&program, opts);
        return ExitCode::SUCCESS;
    }

    let check_lockedhint_and_exit = matches.opt_present("c");
    let ignore_already_set_lockedhint = matches.opt_present("f");

    // option of a tuple containing the locker program's executable name and command line args
    let locker: Option<(String, Vec<String>)> = match check_lockedhint_and_exit {
        true => { None },
        false => {
            let locker_program: String = if !matches.free.is_empty() { //if we have any non-parsed arguments
                matches.free[0].clone() //treat the first of them as the program to execute
            } else { //otherwise
                print_usage(&program, opts); //return an error message
                return ExitCode::SUCCESS;
            };

            let locker_args: Vec<String> = matches.free[1..].to_vec();
            Some((locker_program,locker_args))
        },
    };

    let main_loop = glib::MainLoop::new(None, false);

    let (handle_tx, handle_rx): (mpsc::Sender<BusConnectionData>, mpsc::Receiver<BusConnectionData>) = mpsc::channel();

    let handle_tx2 = handle_tx.clone();
    let logind_watcher_id = gio::bus_watch_name(gio::BusType::System, "org.freedesktop.login1", gio::BusNameWatcherFlags::empty(), move |connection, _name, owner| {

        //println!("Appeared: owned by {}", owner);
        let data = BusConnectionData {
            connection: Some(connection),
            owner: Some(owner.to_string()),
        };
        handle_tx.send(data).unwrap();
    },
    move |_connection, _name| {

        //println!("Vanished");
        let data = BusConnectionData {
            connection: None,
            owner: None,
        };
        handle_tx2.send(data).unwrap();
    }
    );

    let ml = main_loop.clone();

    // this whole thread has to avoid using unwrap or expect, because we have to terminate the main
    // loop upon exiting and thus can't panic
    let work_thread = thread::spawn(move || {

        let bus_data = match handle_rx.recv() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Unable to receive data from the callback: {}",e);
                ml.quit();
                return 1;
            },
        };

        match bus_data.connection {
            None => {
                eprintln!("Unable to get a D-Bus connection");
                ml.quit();
                return 1;
            },
            Some(c) => {

                let owner = match bus_data.owner {
                    Some(v) => v,
                    None => {
                        eprintln!("No bus owner provided!");
                        ml.quit();
                        return 1;
                    },
                };

                let object_path = match get_current_session_object_path(&c,&owner) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("Unable to get object path: {}",e);
                        ml.quit();
                        return 1;
                    },
                };
                let (_session_state, locked_hint) = match get_session_state(&c,&owner,&object_path) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("Unable to get session state: {}",e);
                        ml.quit();
                        return 1;
                    },
                };

                if check_lockedhint_and_exit {
                    println!( "{}", match locked_hint {
                        false => "FALSE",
                        true => "TRUE",
                    });
                    ml.quit();
                    return locked_hint as u8; //0 if false, 1 if true
                } else if locked_hint && !ignore_already_set_lockedhint {
                    println!( "This session already has LockedHint set." );
                    ml.quit();
                    return 1;
                } else {

                    // get the locker program and args, it can't be None at this point
                    let locker = match locker.clone() {
                        Some(v) => v,
                        None => {
                            eprintln!("No locker program provided!");
                            ml.quit();
                            return 1;
                        },
                    };
                    let mut child = match Command::new(locker.0).args(locker.1).spawn() {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("Unable to start command: {}",e);
                            ml.quit();
                            return 1;
                        },
                    };
                    match set_locked_hint(&c,&owner,&object_path,true) { 
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("Unable to set LockedHint: {}",e);
                            ml.quit();
                            return 1;
                        },
                    };
                    let status = match child.wait() {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("Unable to get return code for the locker: {}",e);
                            ml.quit();
                            return 1;
                        },
                    };
                    if status.code() == Some(0) {
                        //Some(v) indicates that the program returned normally with exit code v,
                        //None indicates that the program was terminated by a signal
                        match set_locked_hint(&c,&owner,&object_path,false) {
                            Ok(v) => v,
                            Err(e) => {
                                eprintln!("Unable to clear LockedHint: {}",e);
                                ml.quit();
                                return 1;
                            },
                        };
                    }
                    ml.quit();
                    return 0; //everything ended well
                }
            },
        }
    });

    main_loop.run();

    let result = work_thread.join().unwrap();

    gio::bus_unwatch_name(logind_watcher_id);

    if result == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(result)
    }
}
