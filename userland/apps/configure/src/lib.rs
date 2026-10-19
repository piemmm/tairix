//! TAIRiX `configure` — read and set the machine's settings (`plans/APPS.md`).
//!
//! The `sysctl`-shaped settings command: with no operand it lists every
//! setting of the closed registry with its current value; with a key it
//! shows that setting; with a key and a value it changes it. The machine
//! configuration (`system.conf`) is the shared `lib/sysconfig` engine's
//! document — its grammar, key registry, fail-closed parse, and canonical
//! render — and every reader reads it through the same engine, so this tool
//! and those readers can never diverge.
//!
//! Each document has two layers, the image's default and the
//! administrator's, which replaces it whole once it exists
//! (`tairix_abi::driver_store::SystemConfigFile::layers`). A change is made
//! by the settings service, the documents' one writer, to the document as it
//! stands; the kernel admits only a caller holding `CAP_SETTINGS_WRITE`, so
//! only an elevated administrator changes a setting.
//!
//! # Two registries, one command line
//!
//! A key name resolves against the flat `lib/sysconfig` registry first and,
//! failing that, against the per-interface `<iface>.<suffix>` registry of
//! the network configuration (`network.conf`, `lib/netconfig`). The flat
//! registry always wins, so no interface alias can ever shadow a machine
//! setting — and the two name sets are pinned disjoint by a test over both
//! registries rather than left to the accident that none collides today.
//!
//! Both registries are settable. An invocation names keys from either; every
//! pair is checked against the documents as they stand before the service is
//! asked for anything, and only a document the invocation actually names is
//! changed. The per-interface registry has no defaults to fall back to, so
//! it needs a spelling for **unset**: the empty value. No key in that
//! registry accepts an empty value — the engine pins that — which is what
//! makes the spelling unambiguous, and it is what lets one invocation move an
//! interface from a static address to DHCP (`configure wan.ipv4.method dhcp
//! wan.ipv4.address "" wan.ipv4.gateway ""`), a change neither half of which
//! is a consistent document on its own.
//!
//! The network configuration names the machine's hardware and its
//! addresses, so it is read through the service too, and only by an elevated
//! administrator; any other caller's listing shows the machine settings and
//! says why the network ones are missing.
//!
//! # What this crate is
//!
//! The pure, host-testable core of the tool: the [`parse`]r that maps a
//! command line to a [`Command`], and the [`run`] engine that executes it
//! against injected seams:
//!
//! * [`Documents`] — read a document, or ask the settings service to change
//!   one (the `Run` binary wires the VFS and the service; tests wire the
//!   service's own engine over an in-memory volume).
//! * [`NetPolicy`] — hand a saved change to the running network stack.
//! * [`Output`] — write listings and values to the terminal.
//! * `HelpSource` (from `lib/help`) — the tool's own bundle `Help/` tree,
//!   rendered by the `-h`/`-?`/`--help` switches through the one shared
//!   engine (never embedded help text).
//!
//! # Fail closed
//!
//! An unknown option or an extra operand is a usage error that changes
//! nothing; an unknown key or a value outside its key's closed set is
//! refused with the valid choices stated; a document the shared engine
//! cannot fully parse refuses a *set* outright (never a guessed merge) while
//! a *list*/*show* reports the malformation honestly. A refused read or
//! change surfaces the underlying [`Errno`] — a permission denial changes
//! nothing and states its reason. No panic, no partial application.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`); the dependencies are the audited `lib/abi` crate,
//! the shared `lib/help` engine, and the two configuration engines, so this
//! userland tool never links a kernel or driver crate. No `unsafe`, and no
//! `unwrap`/`expect`/`panic!` in production paths; nothing writes to fd 3
//! (`stdinfo`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::driver_store::SystemConfigFile;
use tairix_abi::net_ipc::{if_name_str, NetBondConfigMsg, NetInterfaceConfigMsg, NetworkSettings};
use tairix_abi::Errno;
use tairix_help::{own_short_help, HelpSource};
use tairix_netconfig::{IfaceKey, InterfaceConfigPlan, NetworkConfig};
use tairix_sysconfig::{Key, SystemConfig};
use tairix_util::conf::ValueShape;

/// The usage banner a usage error is reported with, and the fallback the
/// short-help switches print when `configure`'s own Help tree is
/// unavailable.
pub const USAGE: &str = "usage: configure [<key> [<value> [<key> <value>]...]] [-h | -?]";

/// Most `<key> <value>` pairs one invocation may set.
///
/// Both registries are closed, so every key of both at once is the widest an
/// invocation can meaningfully be; a longer command line names a key twice
/// and is refused rather than applied in some order. Derived from the two
/// registries rather than picked, so neither can outgrow it.
pub const MAX_PAIRS: usize =
    tairix_sysconfig::Key::ALL.len() + tairix_netconfig::MAX_INTERFACES * IfaceKey::ALL.len();

/// One thing the `configure` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command<'a> {
    /// List every registry setting and its current value.
    List,
    /// Show one setting's current value.
    Show(&'a str),
    /// Set each named setting to the value beside it, together.
    ///
    /// Several pairs rather than one, because each document is changed all
    /// or none: applying a group of changes one invocation at a time could
    /// leave it holding half of them if a later one were refused.
    Set(Vec<(&'a str, &'a str)>),
    /// Render `configure`'s own short help (`-h`/`-?`/`--help`) through
    /// the same engine as any other command's short help (plans/APPS.md).
    Help,
}

/// Why a command line or an operation was refused.
///
/// Every variant is a fail-closed refusal: nothing was changed, and the
/// rendered message states the reason (and, for a value refusal, the valid
/// choices).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigureError {
    /// The command line was not understood.
    Usage,
    /// The named key is outside the closed registry.
    UnknownKey,
    /// The value is outside the named key's closed set.
    InvalidValue(Key),
    /// A per-interface setting was refused: the value is outside the key's
    /// set, the alias is malformed, or the store already declares as many
    /// interfaces as it may.
    InterfaceRefused(IfaceKey, tairix_netconfig::ConfigError),
    /// The edited network document does not hold together (a static method
    /// with no address, a bond with too few members, a document that would
    /// outgrow the store bound). Nothing was written.
    NetworkInconsistent(tairix_netconfig::ConfigError),
    /// The machine configuration could not be fully parsed by the shared
    /// engine; a set refuses rather than guess at a merge.
    Malformed(tairix_sysconfig::ParseError),
    /// The machine configuration could not be read.
    Read(Errno),
    /// The network configuration could not be read.
    NetworkRead(Errno),
    /// The network configuration could not be fully parsed by the shared
    /// engine; a listing reports the malformation rather than showing a
    /// document it did not understand.
    NetworkMalformed(tairix_netconfig::ParseError),
    /// The settings service did not change the machine configuration (e.g.
    /// the caller may not change the machine's settings).
    Write(Errno),
    /// The settings service did not change the network configuration.
    NetworkWrite(Errno),
    /// The terminal output could not be delivered.
    Output(Errno),
}

impl fmt::Display for ConfigureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => f.write_str("command line not understood"),
            Self::UnknownKey => f.write_str("unknown setting (run `configure` to list them)"),
            Self::InvalidValue(key) => match key.shape() {
                ValueShape::Closed(values) => {
                    write!(f, "invalid value for {}; valid:", key.name())?;
                    for value in values {
                        write!(f, " {value}")?;
                    }
                    Ok(())
                }
                ValueShape::Free(form) => {
                    write!(f, "invalid value for {}; expected {form}", key.name())
                }
            },
            Self::InterfaceRefused(key, err) => match (err, key.shape()) {
                (tairix_netconfig::ConfigError::InvalidValue, ValueShape::Closed(values)) => {
                    write!(f, "invalid value for {}; valid:", key.name())?;
                    for value in values {
                        write!(f, " {value}")?;
                    }
                    Ok(())
                }
                (tairix_netconfig::ConfigError::InvalidValue, ValueShape::Free(form)) => {
                    write!(f, "invalid value for {}; expected {form}", key.name())
                }
                _ => write!(f, "{}: {err}", key.name()),
            },
            Self::NetworkInconsistent(err) => {
                write!(
                    f,
                    "the network configuration would not hold together: {err}"
                )
            }
            Self::Malformed(err) => write!(f, "the machine settings are not understood: {err}"),
            Self::Read(err) => write!(f, "cannot read the machine settings: {err}"),
            Self::NetworkRead(err) => write!(f, "cannot read the network settings: {err}"),
            Self::NetworkMalformed(err) => {
                write!(f, "the network settings are not understood: {err}")
            }
            Self::Write(err) => write!(f, "the machine settings were not changed: {err}"),
            Self::NetworkWrite(err) => write!(f, "the network settings were not changed: {err}"),
            Self::Output(err) => write!(f, "cannot write output: {err}"),
        }
    }
}

/// The machine's two settings documents, as `configure` reaches them.
///
/// The `Run` binary reads the machine configuration's layers itself and asks
/// the settings service for the network one and for every change; tests wire
/// the service's own engine over an in-memory volume.
pub trait Documents {
    /// `which` as it stands, empty where neither of its layers exists.
    ///
    /// # Errors
    ///
    /// Any refusal — notably [`Errno::PermissionDenied`] for the network
    /// configuration, which names this machine's hardware and addresses and
    /// only an elevated administrator reads.
    fn read(&self, which: SystemConfigFile) -> Result<String, Errno>;

    /// Ask the settings service to set each of `settings` in `which`, all or
    /// none, answering the document as the change left it.
    ///
    /// # Errors
    ///
    /// The service's refusal — notably [`Errno::PermissionDenied`] when the
    /// caller may not change the machine's settings. Nothing was written.
    fn change(&self, which: SystemConfigFile, settings: &[(&str, &str)]) -> Result<String, Errno>;
}

/// Applies the stack-wide `net.*` policy to the running network stack.
///
/// A saved change takes effect at the next boot; the running stack only
/// learns of it when the policy is delivered over its capability-gated admin
/// endpoint. The `Run` binary backs this with that call; tests wire a
/// recorder. It is a seam rather than a direct call so the tool stays
/// host-testable, and it grants nothing: the kernel gates the endpoint on the
/// caller's `CAP_NET_ADMIN`.
pub trait NetPolicy {
    /// Deliver `settings` to the running network stack.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the endpoint raises — notably
    /// [`Errno::PermissionDenied`] when the caller does not hold
    /// `CAP_NET_ADMIN`, or [`Errno::NotFound`] when no network stack is
    /// running. The change has already been saved either way.
    fn apply(&self, settings: NetworkSettings) -> Result<(), Errno>;

    /// The machine's usable physical RAM in bytes, or zero when the
    /// figure is not available.
    ///
    /// A `net.*` *capacity* is derived from the machine rather than
    /// written down, so rendering this document into a policy needs the
    /// machine as well as the document. It sits on this seam because the
    /// engine performs no I/O of its own, and because the boot-time
    /// deliverer reads the same ungated total — the two must agree about
    /// the same document or a live edit and the next boot would differ.
    fn machine_ram_bytes(&self) -> u64;

    /// Ask the running stack to adopt one managed interface's configuration
    /// — the same framed message the device manager delivers at boot, so a
    /// live edit and a boot-time read cannot mean different things.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the endpoint raises, notably [`Errno::NotFound`] when
    /// the interface has not bound to a device yet.
    fn apply_interface(&self, config: &NetInterfaceConfigMsg) -> Result<(), Errno>;

    /// Compose (or recompose) a bond interface in the running stack.
    ///
    /// # Errors
    ///
    /// As [`apply_interface`](Self::apply_interface); [`Errno::NotFound`]
    /// additionally means a declared member has not bound yet.
    fn apply_bond(&self, config: &NetBondConfigMsg) -> Result<(), Errno>;
}

pub use tairix_util::output::Output;

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `configure [<key> [<value>]] [-h | -?]`:
///
/// * `-h` / `-?` / `--help` — the reserved short-help switches
///   (plans/APPS.md; they win immediately).
/// * no operand — list every setting.
/// * one operand — show that setting.
/// * two operands — set that setting.
/// * anything else — a [`ConfigureError::Usage`] error: the tool defines
///   no other options.
///
/// # Errors
///
/// [`ConfigureError::Usage`] for any input outside the grammar above.
pub fn parse<'a>(args: &[&'a str]) -> Result<Command<'a>, ConfigureError> {
    let mut operands: Vec<&'a str> = Vec::new();
    for arg in args {
        match *arg {
            "-h" | "-?" | "--help" => return Ok(Command::Help),
            other if other.starts_with('-') => return Err(ConfigureError::Usage),
            other => operands.push(other),
        }
    }
    match operands.len() {
        0 => Ok(Command::List),
        1 => Ok(Command::Show(operands[0])),
        // An odd count past the first leaves a key with no value, which is
        // a command line that says nothing rather than one to guess at.
        len if len % 2 == 1 => Err(ConfigureError::Usage),
        len if len / 2 > MAX_PAIRS => Err(ConfigureError::Usage),
        _ => Ok(Command::Set(
            operands
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| (pair[0], pair[1]))
                .collect(),
        )),
    }
}

/// Execute `command` against the injected seams.
///
/// # Errors
///
/// The [`ConfigureError`] naming the refusal; nothing was changed and
/// nothing partial was written.
pub fn run(
    command: Command<'_>,
    locale: Option<&str>,
    documents: &dyn Documents,
    policy: &dyn NetPolicy,
    help: &dyn HelpSource,
    output: &dyn Output,
    diagnostics: &dyn Output,
) -> Result<(), ConfigureError> {
    match command {
        Command::Help => {
            // The tool's own Help document through the one shared engine;
            // the usage banner stands in when no document can be served (a
            // build without the bundle's documents) so `-h` never fails.
            let bytes = own_short_help(help, locale, "configure")
                .unwrap_or_else(|| format!("{USAGE}\n").into_bytes());
            output.write_all(&bytes).map_err(ConfigureError::Output)
        }
        Command::List => list(documents, output, diagnostics),
        Command::Show(name) => {
            let text = match resolve(name)? {
                Named::System(key) => format!("{}\n", load(documents)?.render_value(key)),
                // An unset per-interface key has no value to show: the
                // document holds only what was written, so the answer is
                // the empty one rather than a default this registry does
                // not have.
                Named::Interface(iface, key) => format!(
                    "{}\n",
                    load_network(documents)?
                        .interface(iface)
                        .and_then(|found| found.render_value(key))
                        .unwrap_or_default()
                ),
            };
            output
                .write_all(text.as_bytes())
                .map_err(ConfigureError::Output)
        }
        Command::Set(pairs) => set(&pairs, documents, policy, diagnostics),
    }
}

/// What a listing says in place of the network settings when the caller may
/// not read them.
const NETWORK_NOT_SHOWN: &str =
    "the network settings are not shown: only an elevated administrator reads them\n";

/// List every flat-registry setting, then every per-interface one the network
/// configuration holds.
///
/// A caller that may not read the network configuration still gets the
/// machine's settings, and is told why the rest are missing.
fn list(
    documents: &dyn Documents,
    output: &dyn Output,
    diagnostics: &dyn Output,
) -> Result<(), ConfigureError> {
    let config = load(documents)?;
    let mut text = String::new();
    for key in Key::ALL {
        text.push_str(key.name());
        text.push(' ');
        text.push_str(&config.render_value(*key));
        text.push('\n');
    }
    // Only the per-interface keys the document actually holds: that registry
    // has no defaults to print, and a wall of unset keys would say nothing.
    let refused = match load_network(documents) {
        Ok(network) => {
            for iface in network.interfaces() {
                for key in IfaceKey::ALL {
                    if let Some(value) = iface.render_value(*key) {
                        text.push_str(&iface.name);
                        text.push('.');
                        text.push_str(key.name());
                        text.push(' ');
                        text.push_str(&value);
                        text.push('\n');
                    }
                }
            }
            false
        }
        Err(ConfigureError::NetworkRead(Errno::PermissionDenied)) => true,
        Err(err) => return Err(err),
    };
    output
        .write_all(text.as_bytes())
        .map_err(ConfigureError::Output)?;
    if refused {
        diagnostics
            .write_all(NETWORK_NOT_SHOWN.as_bytes())
            .map_err(ConfigureError::Output)?;
    }
    Ok(())
}

/// Change each named setting, then hand what was saved to the running stack.
fn set(
    pairs: &[(&str, &str)],
    documents: &dyn Documents,
    policy: &dyn NetPolicy,
    diagnostics: &dyn Output,
) -> Result<(), ConfigureError> {
    // Every pair is resolved and checked against the documents as they stand
    // before the service is asked for anything, so a command line naming an
    // unknown key or an invalid value changes nothing at all, and the refusal
    // says exactly which.
    let named = resolve_all(pairs)?;
    let (machine_pairs, network_pairs): (Vec<_>, Vec<_>) = pairs
        .iter()
        .zip(&named)
        .partition(|(_, (key, _))| matches!(key, Named::System(_)));
    let machine_pairs: Vec<(&str, &str)> =
        machine_pairs.into_iter().map(|(pair, _)| *pair).collect();
    let network_pairs: Vec<(&str, &str)> =
        network_pairs.into_iter().map(|(pair, _)| *pair).collect();
    if !machine_pairs.is_empty() {
        check_machine(&named, documents)?;
    }
    let current = if network_pairs.is_empty() {
        None
    } else {
        Some(check_network(&named, documents)?)
    };

    // The two documents are changed by two requests with no transaction
    // between them, so a command naming both changes them in order and
    // reports the first refusal — both having been checked, a refusal here
    // is the service's answer and not a half-understood intent.
    let machine = if machine_pairs.is_empty() {
        None
    } else {
        Some(
            documents
                .change(SystemConfigFile::System, &machine_pairs)
                .map_err(ConfigureError::Write)?,
        )
    };
    let network = match current {
        None => None,
        Some(current) => match documents.change(SystemConfigFile::Network, &network_pairs) {
            Ok(written) => Some((current, written)),
            Err(err) => {
                // Saying only that the network half failed would leave the
                // reader believing nothing was written.
                if machine.is_some() {
                    let _ = diagnostics.write_all(SAVED_MACHINE_ONLY.as_bytes());
                }
                return Err(ConfigureError::NetworkWrite(err));
            }
        },
    };

    // Saving a change is half of it: the running stack learns of it over its
    // own admin surface, from the document the service wrote. Applying is a
    // separate, refusable action — a refusal (no stack running, or no
    // `CAP_NET_ADMIN`) leaves the saved setting standing for the next boot
    // and is reported rather than fatal.
    let mut notice = String::new();
    if let Some(written) = machine.as_deref() {
        let keys = machine_network_keys(&named);
        if !keys.is_empty() {
            let delivered = SystemConfig::parse(written)
                .map_err(|_| Errno::OutOfRange)
                .and_then(|config| {
                    policy.apply(config.network_settings(policy.machine_ram_bytes()))
                });
            if let Err(err) = delivered {
                notice.push_str(&deferred_notice(&keys, err));
            }
        }
    }
    if let Some((current, written)) = network {
        match NetworkConfig::parse(&written) {
            Ok(after) => notice.push_str(&apply_interfaces(&current, &after, policy)),
            Err(_) => notice.push_str(NETWORK_NOT_TOLD),
        }
    }
    notice.push_str(&audio_notice(&named));
    if notice.is_empty() {
        return Ok(());
    }
    // A diagnostic, so it never lands in the stdout a script parses.
    diagnostics
        .write_all(notice.as_bytes())
        .map_err(ConfigureError::Output)
}

/// Resolve every `<key> <value>` pair against the two registries, refusing a
/// key named twice.
///
/// One pass before anything is loaded, so a command line that says two
/// things about one setting — there is no order in which both are honoured —
/// changes nothing.
fn resolve_all<'a>(
    pairs: &[(&'a str, &'a str)],
) -> Result<Vec<(Named<'a>, &'a str)>, ConfigureError> {
    let mut named: Vec<(Named<'a>, &'a str)> = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let key = resolve(name)?;
        if named.iter().any(|(seen, _)| *seen == key) {
            return Err(ConfigureError::Usage);
        }
        named.push((key, value));
    }
    Ok(named)
}

/// Check every flat-registry pair against the machine configuration as it
/// stands.
fn check_machine(
    named: &[(Named<'_>, &str)],
    documents: &dyn Documents,
) -> Result<(), ConfigureError> {
    let mut config = load(documents)?;
    for (key, value) in named {
        let Named::System(key) = key else {
            continue;
        };
        config
            .set(*key, value)
            .map_err(|_| ConfigureError::InvalidValue(*key))?;
    }
    Ok(())
}

/// Check every per-interface pair against the network configuration as it
/// stands, answering that configuration.
///
/// The edits accumulate on one draft and are checked whole at the commit,
/// because a consistency rule spanning keys — moving an interface from a
/// static address to DHCP — has no ordering in which each half alone is a
/// document the parser would accept.
fn check_network(
    named: &[(Named<'_>, &str)],
    documents: &dyn Documents,
) -> Result<NetworkConfig, ConfigureError> {
    let current = load_network(documents)?;
    let mut draft = current.edit();
    for (key, value) in named {
        let Named::Interface(iface, key) = key else {
            continue;
        };
        draft
            .apply(iface, *key, value)
            .map_err(|err| ConfigureError::InterfaceRefused(*key, err))?;
    }
    draft
        .commit()
        .map_err(ConfigureError::NetworkInconsistent)?;
    Ok(current)
}

/// The `net.*` keys this invocation set, which is what a refused live apply
/// names in its notice.
fn machine_network_keys(named: &[(Named<'_>, &str)]) -> Vec<Key> {
    named
        .iter()
        .filter_map(|(key, _)| match key {
            Named::System(key) if key.is_network() => Some(*key),
            Named::System(_) | Named::Interface(..) => None,
        })
        .collect()
}

/// Ask the running stack to adopt the interfaces the edit actually changed,
/// answering with the notice a refusal earns (empty when everything landed).
///
/// Only what changed: the plan an interface implies is compared either side
/// of the edit, so an interface the command line did not touch is not
/// reconfigured, and one it did — including a member whose bond took it over
/// — is.
fn apply_interfaces(
    current: &NetworkConfig,
    edited: &NetworkConfig,
    policy: &dyn NetPolicy,
) -> String {
    let before = InterfaceConfigPlan::of(current);
    let after = InterfaceConfigPlan::of(edited);
    let mut notice = String::new();
    for msg in &after.messages {
        if before.message_for(&msg.alias).as_ref() == Some(msg) {
            continue;
        }
        if let Err(err) = policy.apply_interface(msg) {
            notice.push_str(&interface_notice(&msg.alias, err));
        }
    }
    for bond in &after.bonds {
        if before.bond_for(&bond.alias).as_ref() == Some(bond) {
            continue;
        }
        if let Err(err) = policy.apply_bond(bond) {
            notice.push_str(&interface_notice(&bond.alias, err));
        }
    }
    // An interface the edit removed keeps running with the configuration the
    // stack was last given: the admin surface carries no "forget this
    // interface" message, so saying so is the only honest answer.
    for msg in &before.messages {
        if after.message_for(&msg.alias).is_none() {
            notice.push_str(&removed_notice(&msg.alias));
        }
    }
    for reject in &after.rejected {
        // Only one this edit broke: an interface that was already
        // unbindable is not this invocation's news, and calling it "saved"
        // would claim the run had touched it.
        if !before.rejected.contains(reject) {
            notice.push_str(&rejected_notice(reject));
        }
    }
    notice
}

/// The notice a saved-but-not-applied interface change reports.
fn interface_notice(alias: &[u8; tairix_abi::net_ipc::IF_NAME_LEN], err: Errno) -> String {
    format!(
        "{}: saved; the running network stack did not accept it ({err}); it applies at next \
         boot\n",
        if_name_str(alias)
    )
}

/// The notice an interface dropped from the document earns: the store no
/// longer declares it, but the stack has no message that would retire it.
fn removed_notice(alias: &[u8; tairix_abi::net_ipc::IF_NAME_LEN]) -> String {
    format!(
        "{}: removed from the configuration; the running network stack keeps it until next \
         boot\n",
        if_name_str(alias)
    )
}

/// The notice an interface the plan refuses earns: it is saved, but nothing
/// can bind it to hardware, so it will never come up.
fn rejected_notice(alias: &[u8; tairix_abi::net_ipc::IF_NAME_LEN]) -> String {
    format!(
        "{}: saved; it has neither match.mac nor match.node, so no device can be bound to it\n",
        if_name_str(alias)
    )
}

/// What a run reports when the machine document was written and the network
/// one was then refused: the two are separate documents with nothing spanning
/// them, so which half stands has to be said rather than inferred.
const SAVED_MACHINE_ONLY: &str = "the machine settings were saved; the network settings were not\n";

/// What a run reports when the service's answer to a network change cannot be
/// read back, so the running stack cannot be told what was saved.
const NETWORK_NOT_TOLD: &str =
    "the network settings were saved; the running network stack was not told, and takes them at next boot\n";

/// The notice a saved-but-not-applied `net.*` change reports: the settings
/// are persisted, the running stack did not take them, and why.
fn deferred_notice(keys: &[Key], err: Errno) -> String {
    let mut named = String::new();
    for key in keys.iter().filter(|key| key.is_network()) {
        if !named.is_empty() {
            named.push_str(", ");
        }
        named.push_str(key.name());
    }
    format!(
        "{named}: saved; the running network stack did not accept it ({err}); it applies at next \
         boot\n"
    )
}

/// The notice a saved `audio.*` change reports: the audio service holds no
/// filesystem capability, so the device manager hands it the baseline when
/// it next reads the store, and `audioctl` is what changes the running
/// machine. Empty when no audio key was named.
fn audio_notice(named: &[(Named<'_>, &str)]) -> String {
    let mut keys = String::new();
    for (key, _) in named {
        if let Named::System(key) = key {
            if key.is_audio() {
                if !keys.is_empty() {
                    keys.push_str(", ");
                }
                keys.push_str(key.name());
            }
        }
    }
    if keys.is_empty() {
        return keys;
    }
    format!("{keys}: saved; it applies at next boot, and `audioctl` changes the running machine\n")
}

/// Which registry a key name on the command line belongs to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Named<'a> {
    /// A key of the flat machine-configuration registry.
    System(Key),
    /// An interface alias and a key of the per-interface registry.
    Interface(&'a str, IfaceKey),
}

/// Resolve a key name against the two registries, flat one first.
///
/// The order is what makes a machine setting unshadowable: an interface
/// alias is operator-chosen text, so a store that named one `net` must
/// still never take `net.ipv4.enabled` away from the machine registry.
///
/// # Errors
///
/// [`ConfigureError::UnknownKey`] when the name is in neither registry.
fn resolve(name: &str) -> Result<Named<'_>, ConfigureError> {
    if let Some(key) = Key::from_name(name) {
        return Ok(Named::System(key));
    }
    // The key grammar is the engine's, not a second copy of it here.
    let (iface, key) =
        tairix_netconfig::resolve_name(name).map_err(|_| ConfigureError::UnknownKey)?;
    Ok(Named::Interface(iface, key))
}

/// The network configuration as it stands.
///
/// A document the shared engine cannot fully parse is a
/// [`ConfigureError::NetworkMalformed`] refusal, for the same reason the
/// machine configuration's is: this tool never guesses at a partial intent.
fn load_network(documents: &dyn Documents) -> Result<NetworkConfig, ConfigureError> {
    let text = documents
        .read(SystemConfigFile::Network)
        .map_err(ConfigureError::NetworkRead)?;
    NetworkConfig::parse(&text).map_err(ConfigureError::NetworkMalformed)
}

/// The machine configuration as it stands.
///
/// A document the shared engine cannot fully parse is a
/// [`ConfigureError::Malformed`] refusal: the tool never guesses at a
/// partial intent, and a later set never merges into a document it did not
/// understand.
fn load(documents: &dyn Documents) -> Result<SystemConfig, ConfigureError> {
    let text = documents
        .read(SystemConfigFile::System)
        .map_err(ConfigureError::Read)?;
    SystemConfig::parse(&text).map_err(ConfigureError::Malformed)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use tairix_abi::driver_store::SystemConfigFile;
    use tairix_abi::net_ipc::{
        if_name_str, NetBondConfigMsg, NetInterfaceConfigMsg, NetworkSettings,
    };
    use tairix_abi::origin::{CapabilitySummary, Origin, ProcId, TrustDomain};
    use tairix_abi::settings_ipc::{
        decode_reply, encode_set, SETTINGS_REPLY_MAX, SETTINGS_REQUEST_MAX,
    };
    use tairix_abi::{CapabilityId, Errno};
    use tairix_help::HelpSource;
    use tairix_netconfig::IfaceKey;
    use tairix_settingsd::{DocumentStore, SettingsService};
    use tairix_sysconfig::{Key, SystemConfig};

    use super::{parse, run, Command, ConfigureError, Documents, NetPolicy, Output, USAGE};

    const fn slot(which: SystemConfigFile) -> usize {
        which.as_u8() as usize
    }

    /// Both layers of both documents.
    #[derive(Default)]
    struct Layers {
        administrator: [Option<String>; 2],
        image: [Option<String>; 2],
    }

    impl Layers {
        fn effective(&self, which: SystemConfigFile) -> Option<String> {
            self.administrator[slot(which)]
                .clone()
                .or_else(|| self.image[slot(which)].clone())
        }
    }

    /// The layers as the settings service reaches them.
    struct Volume<'a>(&'a RefCell<Layers>);

    impl DocumentStore for Volume<'_> {
        fn read(&mut self, which: SystemConfigFile) -> Result<Option<String>, Errno> {
            Ok(self.0.borrow().effective(which))
        }

        fn replace(&mut self, which: SystemConfigFile, text: &str) -> Result<(), Errno> {
            self.0.borrow_mut().administrator[slot(which)] = Some(text.to_string());
            Ok(())
        }
    }

    /// The machine: its documents, and the real settings service changing
    /// them for an elevated administrator. `None` models the fresh
    /// installation.
    #[derive(Default)]
    struct Machine {
        layers: RefCell<Layers>,
        read_refusal: [Option<Errno>; 2],
        change_refusal: [Option<Errno>; 2],
    }

    impl Machine {
        fn holding(machine: Option<&str>, network: Option<&str>) -> Self {
            let fresh = Self::default();
            {
                let mut layers = fresh.layers.borrow_mut();
                layers.administrator[slot(SystemConfigFile::System)] = machine.map(String::from);
                layers.administrator[slot(SystemConfigFile::Network)] = network.map(String::from);
            }
            fresh
        }

        fn network(text: &str) -> Self {
            Self::holding(None, Some(text))
        }

        /// The administrator's layer of `which`.
        fn stored(&self, which: SystemConfigFile) -> Option<String> {
            self.layers.borrow().administrator[slot(which)].clone()
        }

        fn stored_network(&self) -> String {
            self.stored(SystemConfigFile::Network).unwrap_or_default()
        }
    }

    fn administrator() -> Origin {
        let mut capabilities = CapabilitySummary::EMPTY;
        capabilities.insert(CapabilityId::SETTINGS_WRITE);
        Origin::new(
            TrustDomain::User,
            1000,
            1000,
            7,
            ProcId::from_raw([7; 16]),
            capabilities,
            0,
        )
    }

    impl Documents for Machine {
        fn read(&self, which: SystemConfigFile) -> Result<String, Errno> {
            match self.read_refusal[slot(which)] {
                Some(err) => Err(err),
                None => Ok(self.layers.borrow().effective(which).unwrap_or_default()),
            }
        }

        fn change(
            &self,
            which: SystemConfigFile,
            settings: &[(&str, &str)],
        ) -> Result<String, Errno> {
            if let Some(err) = self.change_refusal[slot(which)] {
                return Err(err);
            }
            let mut request = vec![0u8; SETTINGS_REQUEST_MAX];
            let len = encode_set(which, settings, &mut request)?;
            let mut reply = vec![0u8; SETTINGS_REPLY_MAX];
            let len = SettingsService::new(Volume(&self.layers), tairix_log::DiscardSink).serve(
                &administrator(),
                &request[..len],
                &mut reply,
            );
            decode_reply(&reply[..len]).map(String::from)
        }
    }

    /// A capturing output fixture.
    #[derive(Default)]
    struct MemOutput {
        bytes: RefCell<Vec<u8>>,
    }

    impl Output for MemOutput {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    impl MemOutput {
        fn text(&self) -> String {
            String::from_utf8(self.bytes.borrow().clone()).expect("utf-8 output")
        }
    }

    /// A recording [`NetPolicy`]: captures each delivered policy and
    /// answers with a scripted result.
    struct MemPolicy {
        applied: RefCell<Vec<NetworkSettings>>,
        interfaces: RefCell<Vec<NetInterfaceConfigMsg>>,
        bonds: RefCell<Vec<NetBondConfigMsg>>,
        result: Result<(), Errno>,
    }

    impl MemPolicy {
        fn accepting() -> Self {
            Self {
                applied: RefCell::new(Vec::new()),
                interfaces: RefCell::new(Vec::new()),
                bonds: RefCell::new(Vec::new()),
                result: Ok(()),
            }
        }

        fn refusing(err: Errno) -> Self {
            Self {
                result: Err(err),
                ..Self::accepting()
            }
        }

        /// The aliases of every interface the run pushed, in order.
        fn pushed(&self) -> Vec<String> {
            self.interfaces
                .borrow()
                .iter()
                .map(|msg| String::from(if_name_str(&msg.alias)))
                .collect()
        }
    }

    impl NetPolicy for MemPolicy {
        fn apply(&self, settings: NetworkSettings) -> Result<(), Errno> {
            self.applied.borrow_mut().push(settings);
            self.result
        }

        fn machine_ram_bytes(&self) -> u64 {
            // A 1 GiB machine, so a derived capacity in an asserted
            // policy is a fixed figure rather than the host's own RAM.
            1024 * 1024 * 1024
        }

        fn apply_interface(&self, config: &NetInterfaceConfigMsg) -> Result<(), Errno> {
            self.interfaces.borrow_mut().push(*config);
            self.result
        }

        fn apply_bond(&self, config: &NetBondConfigMsg) -> Result<(), Errno> {
            self.bonds.borrow_mut().push(*config);
            self.result
        }
    }

    /// A help source with no documents, so the usage banner stands in.
    struct NoHelp;

    impl HelpSource for NoHelp {
        fn locale_dirs(&self) -> Result<Vec<String>, tairix_help::SourceError> {
            Ok(Vec::new())
        }
        fn read(
            &self,
            _locale_dir: &str,
            _file_name: &str,
        ) -> Result<Option<Vec<u8>>, tairix_help::SourceError> {
            Ok(None)
        }
    }

    /// Run `command` on `machine`, giving back the outcome, stdout and the
    /// diagnostic stream.
    fn run_on(
        command: Command<'_>,
        machine: &Machine,
        policy: &MemPolicy,
    ) -> (Result<(), ConfigureError>, String, String) {
        let output = MemOutput::default();
        let diagnostics = MemOutput::default();
        let outcome = run(
            command,
            None,
            machine,
            policy,
            &NoHelp,
            &output,
            &diagnostics,
        );
        (outcome, output.text(), diagnostics.text())
    }

    /// Run a set of `<key> <value>` pairs on `machine`, giving back the
    /// run's outcome and the diagnostic stream it wrote.
    fn set_pairs(
        pairs: &[(&str, &str)],
        machine: &Machine,
        policy: &MemPolicy,
    ) -> (Result<(), ConfigureError>, String) {
        let (outcome, _, diagnostics) = run_on(Command::Set(pairs.to_vec()), machine, policy);
        (outcome, diagnostics)
    }

    #[test]
    fn parse_maps_the_grammar() {
        assert_eq!(parse(&[]), Ok(Command::List));
        assert_eq!(parse(&["os.loginType"]), Ok(Command::Show("os.loginType")));
        assert_eq!(
            parse(&["os.loginType", "graphical"]),
            Ok(Command::Set(alloc::vec![("os.loginType", "graphical")])),
        );
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        // Help wins wherever it appears.
        assert_eq!(parse(&["os.loginType", "-h"]), Ok(Command::Help));
    }

    #[test]
    fn parse_refuses_extra_operands_and_unknown_options() {
        assert_eq!(parse(&["a", "b", "c"]), Err(ConfigureError::Usage));
        assert_eq!(parse(&["--frob"]), Err(ConfigureError::Usage));
        assert_eq!(parse(&["-x"]), Err(ConfigureError::Usage));
    }

    #[test]
    fn list_shows_defaults_for_a_fresh_installation() {
        let (outcome, output, _) =
            run_on(Command::List, &Machine::default(), &MemPolicy::accepting());
        outcome.expect("lists");
        assert_eq!(
            output,
            "os.loginType graphical\n\
             cache.all on\n\
             cache.filesystem auto\n\
             cache.block auto\n\
             cache.transform auto\n\
             cache.semantic auto\n\
             net.ipv4.enabled true\n\
             net.ipv6.enabled true\n\
             net.ipv6.privacy false\n\
             net.tcp.syncookies auto\n\
             net.tcp.keepalive false\n\
             net.tcp.ecn false\n\
             net.sockets.mem auto\n\
             time.servers none\n\
             time.refresh 1d\n\
             time.network on\n\
             time.zone default\n\
             input.mouse.debounce 25\n\
             audio.output auto\n\
             audio.input auto\n\
             audio.level 0dB\n",
        );
    }

    #[test]
    fn show_reports_the_stored_value() {
        let machine = Machine::holding(Some("os.loginType graphical\n"), None);
        let (outcome, output, _) = run_on(
            Command::Show("os.loginType"),
            &machine,
            &MemPolicy::accepting(),
        );
        outcome.expect("shows");
        assert_eq!(output, "graphical\n");
    }

    #[test]
    fn the_image_s_settings_stand_until_the_administrator_s_replace_them() {
        let machine = Machine::default();
        machine.layers.borrow_mut().image[slot(SystemConfigFile::System)] =
            Some(String::from("os.loginType text\n"));
        let (outcome, output, _) = run_on(
            Command::Show("os.loginType"),
            &machine,
            &MemPolicy::accepting(),
        );
        outcome.expect("shows");
        assert_eq!(output, "text\n");
    }

    #[test]
    fn set_asks_the_service_and_its_canonical_render_is_what_stands() {
        let machine = Machine::default();
        let (outcome, output, _) = run_on(
            Command::Set(alloc::vec![("os.loginType", "graphical")]),
            &machine,
            &MemPolicy::accepting(),
        );
        outcome.expect("sets");
        let text = machine
            .stored(SystemConfigFile::System)
            .expect("the administrator's layer was written");
        let config = SystemConfig::parse(&text).expect("canonical render parses");
        assert_eq!(config.render_value(Key::LoginType), "graphical");
        assert_eq!(text, config.render(), "rendered by the engine, whole");
        // Nothing goes to stdout on a successful set (the GNU quiet
        // convention).
        assert_eq!(output, "");
    }

    #[test]
    fn setting_a_net_key_applies_it_to_the_running_stack() {
        let policy = MemPolicy::accepting();
        let (outcome, output, errors) = run_on(
            Command::Set(alloc::vec![("net.tcp.ecn", "true")]),
            &Machine::default(),
            &policy,
        );
        outcome.expect("sets");
        // Saving alone would only take effect at the next boot, and the stack
        // holds no filesystem capability to read the document itself.
        let applied = policy.applied.borrow();
        assert_eq!(applied.len(), 1);
        assert!(applied[0].tcp_ecn);
        // The whole policy travels, not just the changed key, so the stack's
        // view can never drift from the document.
        assert!(applied[0].ipv4_enabled && applied[0].ipv6_enabled);
        assert_eq!(output, "", "a delivered change says nothing");
        assert_eq!(errors, "", "and reports no diagnostic");
    }

    #[test]
    fn the_running_stack_is_told_the_document_the_service_wrote() {
        // The image shipped ECN on; the administrator's first change starts
        // from that document, so what reaches the stack carries it too.
        let machine = Machine::default();
        machine.layers.borrow_mut().image[slot(SystemConfigFile::System)] =
            Some(String::from("net.tcp.ecn true\n"));
        let policy = MemPolicy::accepting();
        let (outcome, _) = set_pairs(&[("net.ipv6.privacy", "true")], &machine, &policy);
        outcome.expect("sets");
        let applied = policy.applied.borrow();
        assert_eq!(applied.len(), 1);
        assert!(
            applied[0].tcp_ecn,
            "the image's setting stands in what was written"
        );
        assert!(applied[0].ipv6_privacy);
    }

    #[test]
    fn setting_a_non_net_key_leaves_the_stack_alone() {
        let policy = MemPolicy::accepting();
        let (outcome, _) = set_pairs(&[("os.loginType", "text")], &Machine::default(), &policy);
        outcome.expect("sets");
        assert!(
            policy.applied.borrow().is_empty(),
            "an os.* key is no business of the network stack"
        );
    }

    #[test]
    fn a_refused_live_apply_keeps_the_saved_setting_and_says_so() {
        let machine = Machine::default();
        // No network stack running (or no CAP_NET_ADMIN): the refusal is an
        // answer about one action, not a failure of the command.
        let (outcome, output, errors) = run_on(
            Command::Set(alloc::vec![("net.ipv6.privacy", "true")]),
            &machine,
            &MemPolicy::refusing(Errno::NotFound),
        );
        outcome.expect("the setting is still saved");
        assert_eq!(
            machine.stored(SystemConfigFile::System).as_deref(),
            Some(
                &*SystemConfig::parse("net.ipv6.privacy true\n")
                    .expect("parses")
                    .render()
            )
        );
        // Loud, not silent: the operator is told the running stack did not
        // take it and when it will — on the diagnostic stream, so a script
        // parsing stdout is unaffected.
        assert_eq!(output, "", "stdout carries no diagnostic");
        assert!(errors.contains("net.ipv6.privacy"), "{errors}");
        assert!(errors.contains("next boot"), "{errors}");
    }

    #[test]
    fn unknown_key_fails_closed_without_touching_the_store() {
        let machine = Machine::holding(Some("os.loginType text\n"), None);
        let (outcome, _) = set_pairs(&[("os.frob", "on")], &machine, &MemPolicy::accepting());
        assert_eq!(outcome, Err(ConfigureError::UnknownKey));
        assert_eq!(
            machine.stored(SystemConfigFile::System).as_deref(),
            Some("os.loginType text\n"),
            "a refused set changes nothing"
        );
    }

    #[test]
    fn invalid_value_names_the_valid_choices() {
        let machine = Machine::default();
        let (outcome, _) = set_pairs(
            &[("os.loginType", "desktop")],
            &machine,
            &MemPolicy::accepting(),
        );
        let err = outcome.expect_err("refused");
        assert_eq!(err, ConfigureError::InvalidValue(Key::LoginType));
        assert_eq!(
            format!("{err}"),
            "invalid value for os.loginType; valid: text graphical",
        );
        assert!(
            machine.stored(SystemConfigFile::System).is_none(),
            "nothing was written"
        );
    }

    #[test]
    fn a_malformed_store_refuses_a_set_rather_than_merging() {
        let machine = Machine::holding(Some("os.unknownKey what\n"), None);
        let (outcome, _) = set_pairs(
            &[("os.loginType", "text")],
            &machine,
            &MemPolicy::accepting(),
        );
        let err = outcome.expect_err("refused");
        assert!(
            matches!(err, ConfigureError::Malformed(refusal) if refusal.line == Some(1)),
            "the refusal names the line it was raised at: {err:?}"
        );
        assert_eq!(
            machine.stored(SystemConfigFile::System).as_deref(),
            Some("os.unknownKey what\n"),
            "the malformed document is left untouched"
        );
    }

    #[test]
    fn store_errors_surface_with_their_errno() {
        let mut machine = Machine::default();
        machine.read_refusal[slot(SystemConfigFile::System)] = Some(Errno::MediumError);
        let (outcome, _, _) = run_on(Command::List, &machine, &MemPolicy::accepting());
        assert_eq!(outcome, Err(ConfigureError::Read(Errno::MediumError)));

        let mut machine = Machine::default();
        machine.change_refusal[slot(SystemConfigFile::System)] = Some(Errno::PermissionDenied);
        let (outcome, _) = set_pairs(
            &[("os.loginType", "graphical")],
            &machine,
            &MemPolicy::accepting(),
        );
        assert_eq!(outcome, Err(ConfigureError::Write(Errno::PermissionDenied)));
        assert_eq!(
            format!("{}", ConfigureError::Write(Errno::PermissionDenied)),
            "the machine settings were not changed: permission denied"
        );
    }

    #[test]
    fn help_falls_back_to_the_usage_banner_without_documents() {
        let (outcome, output, _) =
            run_on(Command::Help, &Machine::default(), &MemPolicy::accepting());
        outcome.expect("help renders");
        assert_eq!(output, format!("{USAGE}\n"));
    }

    /// Every locale's Help document names the settings this registry
    /// defines and the reserved short-help switches (`plans/APPS.md`): the
    /// key tokens are language-neutral, so each translated document must
    /// carry the same keys as the canonical one. The documents are read
    /// from the bundle's own on-disk `Help/` tree — the single source the
    /// image builder plants — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_registry_keys_and_switches() {
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        for locale in tairix_help::REQUIRED_LOCALES {
            let path = format!("{help_root}/{locale}/configure.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for token in ["`os.loginType`", "`-h, -?`"] {
                assert!(
                    text.contains(token),
                    "{locale}/configure.md must document {token}"
                );
            }
            for key in Key::ALL {
                assert!(
                    text.contains(key.name()),
                    "{locale}/configure.md must document {}",
                    key.name()
                );
            }
        }
    }

    #[test]
    fn several_pairs_are_applied_to_one_rendered_document() {
        // What makes a settings surface's Apply atomic: the whole change is
        // one request, so the document is never left holding half of it.
        let machine = Machine::default();
        let (outcome, _) = set_pairs(
            &[
                ("os.loginType", "text"),
                ("cache.all", "off"),
                ("cache.block", "off"),
            ],
            &machine,
            &MemPolicy::accepting(),
        );
        outcome.expect("sets");
        let text = machine.stored(SystemConfigFile::System).expect("written");
        let config = SystemConfig::parse(&text).expect("parses");
        assert_eq!(config.login_type, tairix_sysconfig::LoginType::Text);
        assert_eq!(config.cache_all, tairix_sysconfig::Switch::Off);
        assert_eq!(config.cache_block, tairix_sysconfig::CacheMode::Off);
    }

    #[test]
    fn a_refused_pair_changes_nothing_at_all() {
        // Checked against the documents before the service is asked, so a
        // later bad pair cannot leave the earlier ones standing.
        let machine = Machine::holding(Some("os.loginType graphical\n"), None);
        for pairs in [
            [("cache.all", "off"), ("no.such.key", "x")],
            [("cache.all", "off"), ("cache.block", "wat")],
        ] {
            let (refused, _) = set_pairs(&pairs, &machine, &MemPolicy::accepting());
            assert!(refused.is_err());
            assert_eq!(
                machine.stored(SystemConfigFile::System).as_deref(),
                Some("os.loginType graphical\n")
            );
        }
    }

    /// One managed interface, spelled the way the store holds it.
    const ONE_INTERFACE: &str = "wan.kind ethernet\n\
         wan.match.mac 02:00:00:00:00:01\n\
         wan.ipv4.method static\n\
         wan.ipv4.address 10.0.0.7/24\n\
         wan.ipv4.gateway 10.0.0.1\n";

    #[test]
    fn list_states_both_registries_flat_first() {
        let (outcome, text, _) = run_on(
            Command::List,
            &Machine::network(ONE_INTERFACE),
            &MemPolicy::accepting(),
        );
        outcome.expect("lists");
        let (flat, interfaces) = text
            .split_once("wan.kind")
            .expect("the per-interface lines follow the flat registry");
        // Every flat key, defaults included, and no interface line among
        // them.
        for key in Key::ALL {
            assert!(flat.contains(key.name()), "{} missing: {flat}", key.name());
        }
        assert!(!flat.contains("wan."), "an interface line came first");
        // Then only the keys the document actually holds — that registry
        // has no defaults to print.
        assert!(
            interfaces.contains("wan.ipv4.address 10.0.0.7/24"),
            "{interfaces}"
        );
        assert!(!interfaces.contains("wan.mtu"), "an unset key was printed");
    }

    #[test]
    fn a_listing_by_a_caller_who_may_not_read_the_addressing_says_what_is_missing() {
        // An unelevated caller's listing: the machine settings, and the
        // refusal stated rather than the run failing over it.
        let mut machine = Machine::network(ONE_INTERFACE);
        machine.read_refusal[slot(SystemConfigFile::Network)] = Some(Errno::PermissionDenied);
        let (outcome, text, diagnostics) = run_on(Command::List, &machine, &MemPolicy::accepting());
        outcome.expect("the machine settings still list");
        assert!(text.contains("os.loginType graphical"), "{text}");
        assert!(!text.contains("wan."), "{text}");
        assert!(
            diagnostics.contains("network settings are not shown"),
            "{diagnostics}"
        );

        // Any other refusal is the listing's own failure.
        machine.read_refusal[slot(SystemConfigFile::Network)] = Some(Errno::MediumError);
        let (outcome, _, _) = run_on(Command::List, &machine, &MemPolicy::accepting());
        assert_eq!(
            outcome,
            Err(ConfigureError::NetworkRead(Errno::MediumError))
        );
    }

    #[test]
    fn show_reads_a_per_interface_key_and_answers_nothing_for_an_unset_one() {
        for (key, want) in [
            ("wan.ipv4.address", "10.0.0.7/24\n"),
            ("wan.kind", "ethernet\n"),
            // Unset, and on a declared interface: the document holds only
            // what was written, so there is no value and none is invented.
            ("wan.mtu", "\n"),
            // An interface the document never declared, likewise.
            ("lan0.ipv4.address", "\n"),
        ] {
            let (outcome, output, _) = run_on(
                Command::Show(key),
                &Machine::network(ONE_INTERFACE),
                &MemPolicy::accepting(),
            );
            outcome.expect("shows");
            assert_eq!(output, want, "{key}");
        }
    }

    #[test]
    fn a_name_in_neither_registry_is_refused() {
        for key in ["wan.nonsense", "nonsense", "wan..kind", ".kind"] {
            let (outcome, _, _) = run_on(
                Command::Show(key),
                &Machine::network(ONE_INTERFACE),
                &MemPolicy::accepting(),
            );
            assert_eq!(outcome, Err(ConfigureError::UnknownKey), "{key}");
        }
    }

    #[test]
    fn the_two_registries_name_nothing_in_common() {
        // An interface alias is operator-chosen text, so `net` is a legal
        // one and `net.<suffix>` could in principle collide with a machine
        // key. Pinned here rather than left to the accident that none does
        // today: a new key on either side that collided would be resolved
        // by the flat registry and the interface key made unreachable.
        for key in Key::ALL {
            assert!(
                tairix_netconfig::resolve_name(key.name()).is_err(),
                "{} reads as an interface key too",
                key.name()
            );
        }
    }

    #[test]
    fn a_refused_or_malformed_network_store_fails_closed_and_says_which() {
        let mut machine = Machine::network(ONE_INTERFACE);
        machine.read_refusal[slot(SystemConfigFile::Network)] = Some(Errno::PermissionDenied);
        let (refused, _, _) = run_on(Command::Show("wan.mtu"), &machine, &MemPolicy::accepting());
        assert_eq!(
            refused,
            Err(ConfigureError::NetworkRead(Errno::PermissionDenied))
        );
        assert_eq!(
            format!("{}", refused.expect_err("refused")),
            "cannot read the network settings: permission denied"
        );

        let (malformed, _, _) = run_on(
            Command::Show("wan.mtu"),
            &Machine::network("wan.nonsense 1\n"),
            &MemPolicy::accepting(),
        );
        assert!(matches!(
            malformed,
            Err(ConfigureError::NetworkMalformed(_))
        ));
    }

    #[test]
    fn setting_a_per_interface_key_writes_the_network_store_and_applies_it() {
        let machine = Machine::network(ONE_INTERFACE);
        let policy = MemPolicy::accepting();
        let (outcome, notice) = set_pairs(&[("wan.mtu", "9000")], &machine, &policy);
        outcome.expect("sets");
        assert!(notice.is_empty(), "nothing was deferred: {notice}");

        let stored = machine.stored_network();
        assert!(stored.contains("wan.mtu 9000"), "{stored}");
        // The document is rendered whole, so what was already there stays.
        assert!(stored.contains("wan.ipv4.address 10.0.0.7/24"), "{stored}");
        // The machine configuration names nothing this invocation set, so it
        // is not changed at all.
        assert!(
            machine.stored(SystemConfigFile::System).is_none(),
            "an untouched document was rewritten"
        );
        assert_eq!(policy.pushed(), alloc::vec![String::from("wan")]);
        assert!(policy.applied.borrow().is_empty(), "no net.* policy pushed");
    }

    #[test]
    fn the_first_change_keeps_every_interface_the_image_shipped() {
        // The administrator's layer replaces the image's whole, so a change
        // made to anything but the image's document would drop the rest.
        let machine = Machine::default();
        machine.layers.borrow_mut().image[slot(SystemConfigFile::Network)] = Some(String::from(
            "wan.match.mac 02:00:00:00:00:01\nwan.ipv4.method dhcp\n\
             vwan.match.mac 02:00:00:00:00:02\nvwan.ipv4.method dhcp\n",
        ));
        let policy = MemPolicy::accepting();
        let (outcome, _) = set_pairs(&[("wan.mtu", "1400")], &machine, &policy);
        outcome.expect("sets");
        let stored = machine.stored_network();
        assert!(stored.contains("wan.mtu 1400"), "{stored}");
        assert!(stored.contains("vwan.ipv4.method dhcp"), "{stored}");
        assert_eq!(policy.pushed(), alloc::vec![String::from("wan")]);
    }

    /// The audio service holds no filesystem capability, so a saved
    /// `audio.*` change is the next boot's, and the operator is told where
    /// the running machine is changed instead.
    #[test]
    fn a_saved_audio_key_says_when_it_applies() {
        let machine = Machine::network(ONE_INTERFACE);
        let (outcome, notice) = set_pairs(
            &[("audio.level", "-6dB")],
            &machine,
            &MemPolicy::accepting(),
        );
        outcome.expect("sets");
        assert!(
            machine
                .stored(SystemConfigFile::System)
                .is_some_and(|text| text.contains("audio.level -6dB")),
            "saved"
        );
        assert_eq!(
            notice,
            "audio.level: saved; it applies at next boot, and `audioctl` changes the running \
             machine\n"
        );
        let (_, quiet) = set_pairs(
            &[("cache.all", "off")],
            &Machine::network(ONE_INTERFACE),
            &MemPolicy::accepting(),
        );
        assert!(quiet.is_empty(), "{quiet}");
    }

    #[test]
    fn the_empty_value_unsets_a_key_and_moves_an_interface_to_dhcp() {
        let machine = Machine::network(ONE_INTERFACE);
        let policy = MemPolicy::accepting();
        // Neither half of this is a consistent document on its own, in
        // either order, which is why the whole invocation is one change.
        let (outcome, _) = set_pairs(
            &[
                ("wan.ipv4.method", "dhcp"),
                ("wan.ipv4.address", ""),
                ("wan.ipv4.gateway", ""),
            ],
            &machine,
            &policy,
        );
        outcome.expect("sets");
        let stored = machine.stored_network();
        assert!(stored.contains("wan.ipv4.method dhcp"), "{stored}");
        assert!(!stored.contains("wan.ipv4.address"), "{stored}");
        assert!(!stored.contains("wan.ipv4.gateway"), "{stored}");
        assert_eq!(policy.pushed(), alloc::vec![String::from("wan")]);
    }

    #[test]
    fn a_set_that_would_leave_the_network_document_inconsistent_writes_nothing() {
        let machine = Machine::network(ONE_INTERFACE);
        // Static addressing with the address taken away is exactly what the
        // parser refuses, so it is refused here rather than written.
        let (outcome, _) = set_pairs(
            &[("wan.ipv4.address", "")],
            &machine,
            &MemPolicy::accepting(),
        );
        assert_eq!(
            outcome,
            Err(ConfigureError::NetworkInconsistent(
                tairix_netconfig::ConfigError::InconsistentInterface
            ))
        );
        assert_eq!(
            machine.stored_network(),
            ONE_INTERFACE,
            "the store is untouched"
        );
    }

    #[test]
    fn an_invalid_per_interface_value_names_the_choices_and_changes_nothing() {
        let machine = Machine::network(ONE_INTERFACE);
        let (outcome, _) = set_pairs(
            &[("wan.ipv4.method", "sometimes")],
            &machine,
            &MemPolicy::accepting(),
        );
        let text = outcome.expect_err("refused").to_string();
        for value in tairix_netconfig::Ipv4Method::VALUES {
            assert!(text.contains(value), "{text}");
        }
        assert_eq!(
            machine.stored_network(),
            ONE_INTERFACE,
            "the store is untouched"
        );
    }

    #[test]
    fn a_malformed_alias_states_the_engine_refusal_rather_than_a_value_one() {
        let (outcome, _) = set_pairs(
            &[("wan.bond.primary", "0bad")],
            &Machine::default(),
            &MemPolicy::accepting(),
        );
        assert_eq!(
            outcome,
            Err(ConfigureError::InterfaceRefused(
                IfaceKey::BondPrimary,
                tairix_netconfig::ConfigError::InvalidInterfaceName
            ))
        );
    }

    #[test]
    fn one_invocation_may_set_both_registries() {
        let machine = Machine::network(ONE_INTERFACE);
        let policy = MemPolicy::accepting();
        let (outcome, _) = set_pairs(
            &[("net.ipv6.enabled", "false"), ("wan.mtu", "1400")],
            &machine,
            &policy,
        );
        outcome.expect("sets");
        assert!(
            machine
                .stored(SystemConfigFile::System)
                .unwrap_or_default()
                .contains("net.ipv6.enabled false"),
            "the machine configuration was not written"
        );
        assert!(machine.stored_network().contains("wan.mtu 1400"));
        // Both halves reach the running stack, each over its own message.
        assert_eq!(policy.applied.borrow().len(), 1);
        assert_eq!(policy.pushed(), alloc::vec![String::from("wan")]);
    }

    #[test]
    fn only_the_interfaces_the_edit_changed_are_pushed() {
        let two = "wan.match.mac 02:00:00:00:00:01\n\
                   wan.mtu 1500\n\
                   lan.match.mac 02:00:00:00:00:02\n\
                   lan.mtu 1500\n";
        let policy = MemPolicy::accepting();
        let (outcome, _) = set_pairs(&[("wan.mtu", "9000")], &Machine::network(two), &policy);
        outcome.expect("sets");
        assert_eq!(
            policy.pushed(),
            alloc::vec![String::from("wan")],
            "an untouched interface was reconfigured"
        );
    }

    #[test]
    fn a_refused_interface_apply_keeps_the_saved_setting_and_says_so() {
        let machine = Machine::network(ONE_INTERFACE);
        let (outcome, notice) = set_pairs(
            &[("wan.mtu", "9000")],
            &machine,
            &MemPolicy::refusing(Errno::PermissionDenied),
        );
        outcome.expect("the change was saved");
        assert!(
            machine.stored_network().contains("wan.mtu 9000"),
            "still saved"
        );
        assert!(notice.contains("wan"), "{notice}");
        assert!(notice.contains("next boot"), "{notice}");
    }

    #[test]
    fn an_interface_the_edit_removed_is_reported_as_still_running() {
        let machine = Machine::network(ONE_INTERFACE);
        let policy = MemPolicy::accepting();
        // Clearing every key drops the interface from the document. The
        // stack's admin surface carries no message that retires one, so the
        // tool says so rather than implying the machine is now unaddressed.
        let (outcome, notice) = set_pairs(
            &[
                ("wan.kind", ""),
                ("wan.match.mac", ""),
                ("wan.ipv4.method", ""),
                ("wan.ipv4.address", ""),
                ("wan.ipv4.gateway", ""),
            ],
            &machine,
            &policy,
        );
        outcome.expect("sets");
        assert!(
            !machine.stored_network().contains("wan."),
            "{}",
            machine.stored_network()
        );
        assert!(notice.contains("wan: removed"), "{notice}");
        assert!(policy.pushed().is_empty(), "nothing to push");
    }

    #[test]
    fn an_interface_with_no_hardware_identity_is_saved_and_the_refusal_stated() {
        let machine = Machine::default();
        let (outcome, notice) =
            set_pairs(&[("lan.mtu", "1400")], &machine, &MemPolicy::accepting());
        outcome.expect("sets");
        assert!(machine.stored_network().contains("lan.mtu 1400"));
        assert!(notice.contains("match.mac"), "{notice}");
    }

    #[test]
    fn a_refused_network_write_surfaces_and_leaves_the_stack_alone() {
        let mut machine = Machine::default();
        machine.change_refusal[slot(SystemConfigFile::Network)] = Some(Errno::PermissionDenied);
        let policy = MemPolicy::accepting();
        let (outcome, _) = set_pairs(&[("lan.mtu", "1400")], &machine, &policy);
        assert_eq!(
            outcome,
            Err(ConfigureError::NetworkWrite(Errno::PermissionDenied))
        );
        assert!(
            policy.pushed().is_empty(),
            "a change that was not saved must not be applied"
        );
    }

    #[test]
    fn an_interface_already_unbindable_is_not_reported_as_this_run_s_news() {
        // `lan` was already saved without a hardware identity. An edit to a
        // different interface has not touched it, so calling it "saved"
        // would claim the run had.
        let machine = Machine::network("lan.mtu 1400\nwan.match.mac 02:00:00:00:00:01\n");
        let (outcome, notice) =
            set_pairs(&[("wan.mtu", "9000")], &machine, &MemPolicy::accepting());
        outcome.expect("sets");
        assert!(!notice.contains("lan"), "{notice}");
    }

    #[test]
    fn a_half_landed_cross_store_write_says_which_half_stands() {
        let mut machine = Machine::default();
        machine.change_refusal[slot(SystemConfigFile::Network)] = Some(Errno::PermissionDenied);
        let (outcome, notice) = set_pairs(
            &[
                ("net.ipv6.enabled", "false"),
                ("wan.match.mac", "02:00:00:00:00:01"),
            ],
            &machine,
            &MemPolicy::accepting(),
        );
        assert_eq!(
            outcome,
            Err(ConfigureError::NetworkWrite(Errno::PermissionDenied))
        );
        assert!(
            machine.stored(SystemConfigFile::System).is_some(),
            "the machine half landed"
        );
        assert!(notice.contains("machine settings were saved"), "{notice}");
    }

    #[test]
    fn the_same_interface_key_named_twice_is_a_usage_error() {
        let machine = Machine::network(ONE_INTERFACE);
        let (outcome, _) = set_pairs(
            &[("wan.mtu", "1400"), ("wan.mtu", "9000")],
            &machine,
            &MemPolicy::accepting(),
        );
        assert_eq!(outcome, Err(ConfigureError::Usage));
        assert_eq!(machine.stored_network(), ONE_INTERFACE);
        // The same key on two interfaces is two settings, not a repeat.
        let (outcome, _) = set_pairs(
            &[("wan.mtu", "1400"), ("lan.mtu", "9000")],
            &Machine::network(ONE_INTERFACE),
            &MemPolicy::accepting(),
        );
        outcome.expect("two interfaces, two settings");
    }

    #[test]
    fn the_command_line_grammar_admits_pairs_and_refuses_a_lone_key() {
        assert_eq!(
            parse(&["os.loginType", "text", "cache.all", "off"]),
            Ok(Command::Set(alloc::vec![
                ("os.loginType", "text"),
                ("cache.all", "off")
            ]))
        );
        // An odd operand past the first leaves a key with no value.
        assert_eq!(
            parse(&["os.loginType", "text", "cache.all"]),
            Err(ConfigureError::Usage)
        );
        // And a key named twice has two intents for one setting.
        assert_eq!(
            parse(&["cache.all", "on", "cache.all", "off"]).map(|command| run_on(
                command,
                &Machine::default(),
                &MemPolicy::accepting()
            )
            .0),
            Ok(Err(ConfigureError::Usage))
        );
    }
}
