//! What a Location of this technology is configured with: its declaration,
//! which `Configured` reads, built with the crate's `declaration` feature
//! alone and the code behind the default (ADR-0064, amendments 2026-10-22
//! and 2026-11-26), for the library the surfaces load to carry (open
//! problem 48).

use xcore::settings::{Applies, Kind, Presence, Setting, Settings};

/// Every setting a Location takes beyond its address, where a Receive
/// Location listens as a server and where a Send Location connects as a
/// client.
pub const SETTINGS: &Settings = &Settings {
    technology: env!("CARGO_PKG_NAME"),
    settings: &[
        Setting {
            name: "unit",
            kind: Kind::Integer {
                minimum: 0,
                maximum: 255,
            },
            presence: Presence::Optional,
            meaning: "The unit identifier a Send Location addresses; unit 1 when left out.",
            applies: Applies::Send,
        },
        transport::configured::timeout(
            Applies::Both,
            None,
            "How long a connection is waited for and a peer that stops mid-frame.",
        ),
    ],
};

/// The schemes a Location's address is entered with to choose this technology.
pub const SCHEMES: &[&str] = &["modbus"];
