//! Supply and GPIO bindings: the regulator a consumer's `<name>-supply`
//! property names, and the one GPIO line that switches it.
//!
//! Two regulator shapes are decoded, as the Linux regulator bindings define
//! them (`Documentation/devicetree/bindings/regulator/`): a `regulator-gpio`
//! whose single select line picks between voltage `states`, and a
//! `regulator-fixed` whose single `gpio` enables its output. Anything wider —
//! several select lines, a separate enable line on a selecting regulator, a
//! GPIO specifier that is not the two-cell `<line flags>` form — decodes to
//! `None`, so a consumer never drives a rail it has only half understood.

use crate::{be_u32, phandle_ref, Fdt, Node};

/// The `#gpio-cells` of the one GPIO specifier shape decoded here: a line
/// number and a flags cell.
const GPIO_CELLS: u32 = 2;

/// Bytes of one decoded specifier: the controller phandle plus its cells.
const GPIO_SPECIFIER_BYTES: usize = 4 * (1 + GPIO_CELLS as usize);

/// The flags-cell bit marking a line active-low (`GPIO_ACTIVE_LOW`).
const GPIO_ACTIVE_LOW: u32 = 1;

/// Bytes of one `states` entry: a voltage in microvolts and the select value.
const STATE_BYTES: usize = 8;

/// One GPIO line a specifier names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GpioLine {
    /// Phandle of the controller the line belongs to.
    pub controller: u32,
    /// Line number within that controller.
    pub line: u32,
    /// Whether the line is asserted at its low level.
    pub active_low: bool,
}

impl GpioLine {
    /// The physical level that leaves the line `asserted`.
    #[must_use]
    pub const fn level(self, asserted: bool) -> bool {
        asserted != self.active_low
    }
}

/// A `regulator-gpio` whose one select line picks its output voltage.
#[derive(Copy, Clone, Debug)]
pub struct GpioSelectedRegulator<'a> {
    line: GpioLine,
    states: &'a [u8],
    settle_us: u32,
}

impl GpioSelectedRegulator<'_> {
    /// The select line.
    #[must_use]
    pub const fn line(&self) -> GpioLine {
        self.line
    }

    /// The physical level of the select line that makes the regulator output
    /// `microvolts`, or `None` when no state names that voltage.
    #[must_use]
    pub fn level_for(&self, microvolts: u32) -> Option<bool> {
        states(self.states)
            .find(|&(volts, _)| volts == microvolts)
            .map(|(_, value)| self.line.level(value != 0))
    }

    /// How long the output takes to settle after a change, in microseconds
    /// (`regulator-settling-time-us`; zero when the board declares none).
    #[must_use]
    pub const fn settle_us(&self) -> u32 {
        self.settle_us
    }
}

/// A `regulator-fixed` whose output one GPIO line switches on and off.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GpioEnabledRegulator {
    line: GpioLine,
    startup_us: u32,
    off_on_us: u32,
}

impl GpioEnabledRegulator {
    /// The enable line, its polarity already the binding's
    /// (`enable-active-high`, else active-low).
    #[must_use]
    pub const fn line(&self) -> GpioLine {
        self.line
    }

    /// How long the output takes to rise after it is enabled, in
    /// microseconds (`startup-delay-us`; zero when the board declares none).
    #[must_use]
    pub const fn startup_us(&self) -> u32 {
        self.startup_us
    }

    /// The shortest time the output must stay off before it may be enabled
    /// again, in microseconds (`off-on-delay-us`; zero when undeclared).
    #[must_use]
    pub const fn off_on_us(&self) -> u32 {
        self.off_on_us
    }
}

/// The enabled regulator `consumer`'s `property` (e.g. `vqmmc-supply`)
/// names, or `None` when the property is absent, malformed, or names a
/// missing or disabled node.
#[must_use]
pub fn supply<'a>(fdt: &Fdt<'a>, consumer: &Node<'a>, property: &str) -> Option<Node<'a>> {
    let value = consumer.property(property)?;
    if value.value().len() != 4 {
        return None;
    }
    let node = fdt.node_by_phandle(value.read_be_u32(0).ok()?)?;
    node.is_enabled().then_some(node)
}

/// Decode `node` as a [`GpioSelectedRegulator`]: a `regulator-gpio` with
/// exactly one select line and at least one well-formed voltage state.
#[must_use]
pub fn gpio_selected_regulator<'a>(
    fdt: &Fdt<'a>,
    node: &Node<'a>,
) -> Option<GpioSelectedRegulator<'a>> {
    if !node.is_compatible("regulator-gpio") {
        return None;
    }
    // A separate enable line is a second control the select line alone
    // cannot drive, so the regulator is not described by one line.
    if node.property("enable-gpios").is_some() || node.property("enable-gpio").is_some() {
        return None;
    }
    let line = gpio_line(fdt, node.property("gpios")?.value())?;
    let table = node.property("states")?.value();
    if table.is_empty() || !table.len().is_multiple_of(STATE_BYTES) {
        return None;
    }
    // One line selects between two values; a state asking for another bit
    // needs a line this regulator does not have.
    if states(table).any(|(_, value)| value > 1) {
        return None;
    }
    Some(GpioSelectedRegulator {
        line,
        states: table,
        settle_us: optional_u32(node, "regulator-settling-time-us")?,
    })
}

/// Decode `node` as a [`GpioEnabledRegulator`]: a `regulator-fixed` with one
/// enable line.
///
/// The fixed-regulator binding takes the line's polarity from
/// `enable-active-high` alone (active-low without it) and ignores the
/// specifier's flags cell, as Linux's GPIO quirk for it does.
#[must_use]
pub fn gpio_enabled_regulator(fdt: &Fdt<'_>, node: &Node<'_>) -> Option<GpioEnabledRegulator> {
    if !node.is_compatible("regulator-fixed") {
        return None;
    }
    let specifier = node.property("gpio").or_else(|| node.property("gpios"))?;
    let line = gpio_line(fdt, specifier.value())?;
    Some(GpioEnabledRegulator {
        line: GpioLine {
            active_low: node.property("enable-active-high").is_none(),
            ..line
        },
        startup_us: optional_u32(node, "startup-delay-us")?,
        off_on_us: optional_u32(node, "off-on-delay-us")?,
    })
}

/// Decode a property value holding exactly one two-cell GPIO specifier whose
/// controller declares that shape.
fn gpio_line(fdt: &Fdt<'_>, value: &[u8]) -> Option<GpioLine> {
    if value.len() != GPIO_SPECIFIER_BYTES {
        return None;
    }
    let controller = phandle_ref(be_u32(value, 0)?)?;
    let cells = fdt.node_by_phandle(controller)?.property("#gpio-cells")?;
    if cells.value().len() != 4 || cells.read_be_u32(0).ok()? != GPIO_CELLS {
        return None;
    }
    Some(GpioLine {
        controller,
        line: be_u32(value, 4)?,
        active_low: be_u32(value, 8)? & GPIO_ACTIVE_LOW != 0,
    })
}

/// The `(microvolts, select value)` pairs of a `states` table; a trailing
/// partial entry is not a state.
fn states(table: &[u8]) -> impl Iterator<Item = (u32, u32)> + '_ {
    table
        .as_chunks::<STATE_BYTES>()
        .0
        .iter()
        .map(|&[a, b, c, d, e, f, g, h]| {
            (
                u32::from_be_bytes([a, b, c, d]),
                u32::from_be_bytes([e, f, g, h]),
            )
        })
}

/// A one-cell `u32` property that defaults to zero when absent; `None` when
/// present but not one cell, so a malformed timing is never read as none.
fn optional_u32(node: &Node<'_>, name: &str) -> Option<u32> {
    match node.property(name) {
        None => Some(0),
        Some(property) if property.value().len() == 4 => property.read_be_u32(0).ok(),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::FdtWriter;
    use alloc::vec::Vec;

    const EXPANDER: u32 = 0xb;
    const IO_REGULATOR: u32 = 0x37;
    const CARD_REGULATOR: u32 = 0x38;

    /// The Pi 4's SD supply wiring: a firmware GPIO expander, the
    /// GPIO-selected I/O regulator on its line 4, the GPIO-enabled card
    /// regulator on its line 6, and an SD host naming both.
    fn pi4_like(io_gpio_flags: u32, card_active_high: bool) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("gpio");
        b.prop_str("compatible", "raspberrypi,firmware-gpio");
        b.prop("gpio-controller", &[]);
        b.prop_u32("#gpio-cells", 2);
        b.prop_u32("phandle", EXPANDER);
        b.end_node();
        b.begin_node("regulator-sd-io-1v8");
        b.prop_str("compatible", "regulator-gpio");
        b.prop("gpios", &cells(&[EXPANDER, 4, io_gpio_flags]));
        b.prop("states", &cells(&[1_800_000, 1, 3_300_000, 0]));
        b.prop_u32("regulator-settling-time-us", 5000);
        b.prop_str("status", "okay");
        b.prop_u32("phandle", IO_REGULATOR);
        b.end_node();
        b.begin_node("regulator-sd-vcc");
        b.prop_str("compatible", "regulator-fixed");
        if card_active_high {
            b.prop("enable-active-high", &[]);
        }
        b.prop("gpio", &cells(&[EXPANDER, 6, 0]));
        b.prop_u32("phandle", CARD_REGULATOR);
        b.end_node();
        b.begin_node("mmc@7e340000");
        b.prop_str("compatible", "brcm,bcm2711-emmc2");
        b.prop_u32("vqmmc-supply", IO_REGULATOR);
        b.prop_u32("vmmc-supply", CARD_REGULATOR);
        b.end_node();
        b.end_node();
        b.build()
    }

    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn host<'a>(fdt: &Fdt<'a>) -> Node<'a> {
        fdt.find_compatible("brcm,bcm2711-emmc2")
            .expect("host node")
    }

    #[test]
    fn the_io_regulator_is_selected_by_its_line_and_states() {
        let blob = pi4_like(0, true);
        let fdt = Fdt::new(&blob).expect("fdt");
        let node = supply(&fdt, &host(&fdt), "vqmmc-supply").expect("vqmmc");
        let regulator = gpio_selected_regulator(&fdt, &node).expect("regulator-gpio");
        assert_eq!(
            regulator.line(),
            GpioLine {
                controller: EXPANDER,
                line: 4,
                active_low: false
            }
        );
        assert_eq!(regulator.level_for(1_800_000), Some(true));
        assert_eq!(regulator.level_for(3_300_000), Some(false));
        assert_eq!(regulator.level_for(2_500_000), None);
        assert_eq!(regulator.settle_us(), 5000);
    }

    #[test]
    fn an_active_low_select_line_inverts_every_state() {
        let blob = pi4_like(GPIO_ACTIVE_LOW, true);
        let fdt = Fdt::new(&blob).expect("fdt");
        let node = supply(&fdt, &host(&fdt), "vqmmc-supply").expect("vqmmc");
        let regulator = gpio_selected_regulator(&fdt, &node).expect("regulator-gpio");
        assert_eq!(regulator.level_for(1_800_000), Some(false));
        assert_eq!(regulator.level_for(3_300_000), Some(true));
    }

    #[test]
    fn the_card_regulator_takes_its_polarity_from_enable_active_high_alone() {
        for (active_high, active_low) in [(true, false), (false, true)] {
            let blob = pi4_like(0, active_high);
            let fdt = Fdt::new(&blob).expect("fdt");
            let node = supply(&fdt, &host(&fdt), "vmmc-supply").expect("vmmc");
            let regulator = gpio_enabled_regulator(&fdt, &node).expect("regulator-fixed");
            assert_eq!(regulator.line().line, 6);
            assert_eq!(regulator.line().active_low, active_low);
            assert_eq!(regulator.line().level(true), active_high);
            assert_eq!((regulator.startup_us(), regulator.off_on_us()), (0, 0));
        }
    }

    #[test]
    fn a_shape_the_decoders_do_not_describe_is_refused() {
        let blob = pi4_like(0, true);
        let fdt = Fdt::new(&blob).expect("fdt");
        let host = host(&fdt);
        let io = supply(&fdt, &host, "vqmmc-supply").expect("vqmmc");
        let card = supply(&fdt, &host, "vmmc-supply").expect("vmmc");
        assert!(gpio_enabled_regulator(&fdt, &io).is_none());
        assert!(gpio_selected_regulator(&fdt, &card).is_none());
        assert!(supply(&fdt, &host, "vdd-supply").is_none());
    }

    #[test]
    fn a_disabled_regulator_supplies_nothing() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("regulator");
        b.prop_str("compatible", "regulator-fixed");
        b.prop_str("status", "disabled");
        b.prop_u32("phandle", 5);
        b.end_node();
        b.begin_node("host");
        b.prop_u32("vmmc-supply", 5);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("fdt");
        let host = fdt
            .nodes()
            .map(|n| n.expect("node"))
            .find(|n| n.name() == b"host")
            .expect("host");
        assert!(supply(&fdt, &host, "vmmc-supply").is_none());
    }

    /// A regulator node whose one property `name` is `value`, under a
    /// two-cell GPIO controller, with the fields every decoder needs.
    fn regulator_with(compatible: &str, name: &str, value: &[u8]) -> Vec<u8> {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("gpio");
        b.prop_u32("#gpio-cells", 2);
        b.prop_u32("phandle", EXPANDER);
        b.end_node();
        b.begin_node("regulator");
        b.prop_str("compatible", compatible);
        if name != "gpios" {
            b.prop("gpios", &cells(&[EXPANDER, 4, 0]));
            b.prop("gpio", &cells(&[EXPANDER, 4, 0]));
        }
        if name != "states" {
            b.prop("states", &cells(&[1_800_000, 1, 3_300_000, 0]));
        }
        b.prop(name, value);
        b.end_node();
        b.end_node();
        b.build()
    }

    fn only_regulator<'a>(fdt: &Fdt<'a>) -> Node<'a> {
        fdt.nodes()
            .map(|n| n.expect("node"))
            .find(|n| n.name() == b"regulator")
            .expect("regulator")
    }

    #[test]
    fn malformed_select_regulators_are_refused() {
        let cases: [(&str, Vec<u8>); 6] = [
            // Two select lines.
            ("gpios", cells(&[EXPANDER, 4, 0, EXPANDER, 5, 0])),
            // A controller the specifier cannot be decoded against.
            ("gpios", cells(&[0x99, 4, 0])),
            // A truncated states table.
            ("states", cells(&[1_800_000, 1, 3_300_000])),
            // A state needing a second select line.
            ("states", cells(&[1_800_000, 2])),
            // A separate enable line.
            ("enable-gpios", cells(&[EXPANDER, 7, 0])),
            // A settling time that is not one cell.
            ("regulator-settling-time-us", cells(&[1, 2])),
        ];
        for (name, value) in cases {
            let blob = regulator_with("regulator-gpio", name, &value);
            let fdt = Fdt::new(&blob).expect("fdt");
            assert!(
                gpio_selected_regulator(&fdt, &only_regulator(&fdt)).is_none(),
                "{name} = {value:?}"
            );
        }
    }

    #[test]
    fn a_controller_with_other_cell_counts_is_refused() {
        let mut b = FdtWriter::new();
        b.begin_node("");
        b.begin_node("gpio");
        b.prop_u32("#gpio-cells", 3);
        b.prop_u32("phandle", EXPANDER);
        b.end_node();
        b.begin_node("regulator");
        b.prop_str("compatible", "regulator-fixed");
        b.prop("gpio", &cells(&[EXPANDER, 6, 0]));
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("fdt");
        assert!(gpio_enabled_regulator(&fdt, &only_regulator(&fdt)).is_none());
    }

    #[test]
    fn a_phandle_naming_no_node_finds_none() {
        let blob = pi4_like(0, true);
        let fdt = Fdt::new(&blob).expect("fdt");
        assert!(fdt.node_by_phandle(0).is_none());
        assert!(fdt.node_by_phandle(u32::MAX).is_none());
        assert!(fdt.node_by_phandle(0x1234).is_none());
        assert_eq!(
            fdt.node_by_phandle(EXPANDER).map(|n| n.name()),
            Some(&b"gpio"[..])
        );
    }
}
