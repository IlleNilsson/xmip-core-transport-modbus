//! This technology's declaration: the transport guide's "The declaration",
//! `module/core/capability/transport/doc/adding-a-transport.md`.

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

/// This technology's schemes, as [`transport::Configured::SCHEMES`] defines
/// them.
pub const SCHEMES: &[&str] = &["modbus"];
