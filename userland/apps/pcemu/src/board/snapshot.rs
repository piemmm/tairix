//! The board's part of a snapshot: every chip and adapter, the memory's
//! contents, and the planar's clocks; what each page and port maps to is
//! worked out again from the devices once they are restored.

use super::{Blaster, Board, Expanded, Planar, Ports, Sound};
use crate::chips::pcb186::{Pcb, Space};
use crate::ems;
use crate::snapshot::{check, restore_fitted, save_fitted, In, Out, State, Torn};
use crate::sound::TICKS_PER_SAMPLE;
use crate::time::{CpuClock, Hertz, Nanos, NANOS_PER_SECOND};

crate::snapshot::state!(Blaster { dsp } as built { base, irq });

crate::snapshot::state!(Expanded { board } as built { port, frame, store });

impl State for Sound {
    fn save(&self, out: &mut Out) {
        let Self {
            mixer,
            speaker,
            fm,
            blaster,
        } = self;
        mixer.save(out);
        speaker.save(out);
        save_fitted(fm.as_ref(), out);
        save_fitted(blaster.as_ref(), out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.mixer.restore(input)?;
        self.speaker.restore(input)?;
        restore_fitted(self.fm.as_mut(), input)?;
        restore_fitted(self.blaster.as_mut(), input)
    }
}

impl State for Ports {
    fn save(&self, out: &mut Out) {
        let Self {
            serial,
            parallel,
            game,
            clock,
        } = self;
        for uart in serial {
            save_fitted(uart.as_ref(), out);
        }
        for port in parallel {
            save_fitted(port.as_ref().map(|(lpt, _)| lpt), out);
        }
        save_fitted(game.as_ref(), out);
        save_fitted(clock.as_ref(), out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        for uart in &mut self.serial {
            restore_fitted(uart.as_mut(), input)?;
        }
        for port in &mut self.parallel {
            restore_fitted(port.as_mut().map(|(lpt, _)| lpt), input)?;
        }
        restore_fitted(self.game.as_mut(), input)?;
        restore_fitted(self.clock.as_mut(), input)
    }
}

impl State for Planar {
    fn save(&self, out: &mut Out) {
        let Self {
            memory,
            decode: _,
            pic,
            pit,
            dma,
            ppi,
            keyboard,
            cards,
            fdc,
            hdc,
            sound,
            ports,
            ems,
            rendered,
            now,
            pit_tick,
            refresh_tick,
            nmi_enabled,
            // The debugger's, which no snapshot keeps.
            probe: _,
        } = self;
        memory.save(out);
        pic.save(out);
        pit.save(out);
        dma.save(out);
        ppi.save(out);
        keyboard.save(out);
        out.u32(u32::try_from(cards.len()).unwrap_or(u32::MAX));
        for card in cards {
            card.save(out);
        }
        save_fitted(fdc.as_ref(), out);
        save_fitted(hdc.as_ref(), out);
        sound.save(out);
        ports.save(out);
        save_fitted(ems.as_ref(), out);
        rendered.save(out);
        now.save(out);
        pit_tick.save(out);
        refresh_tick.save(out);
        nmi_enabled.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let Self {
            memory,
            decode: _,
            pic,
            pit,
            dma,
            ppi,
            keyboard,
            cards,
            fdc,
            hdc,
            sound,
            ports,
            ems,
            rendered,
            now,
            pit_tick,
            refresh_tick,
            nmi_enabled,
            probe: _,
        } = self;
        memory.restore(input)?;
        pic.restore(input)?;
        pit.restore(input)?;
        dma.restore(input)?;
        ppi.restore(input)?;
        keyboard.restore(input)?;
        if usize::try_from(input.u32()?).ok() != Some(cards.len()) {
            return Err(Torn::Mismatch);
        }
        for card in cards.iter_mut() {
            card.restore(input)?;
        }
        restore_fitted(fdc.as_mut(), input)?;
        restore_fitted(hdc.as_mut(), input)?;
        sound.restore(input)?;
        ports.restore(input)?;
        restore_fitted(ems.as_mut(), input)?;
        rendered.restore(input)?;
        now.restore(input)?;
        pit_tick.restore(input)?;
        refresh_tick.restore(input)?;
        nmi_enabled.restore(input)?;
        check(*rendered <= *now)
    }
}

impl State for Board {
    fn save(&self, out: &mut Out) {
        let Self {
            clock,
            pcb_clock: _,
            pcb,
            planar,
            waits: _,
            // What translated code reaches, which follows from the rest.
            waits_changed: _,
            pages: _,
            stolen,
            held,
            next_refresh,
            rescheduled,
            intr: _,
            host_pointer,
            // A share is the front-end's to give again: a snapshot keeps none.
            shares: _,
            bus: _,
            fpu,
            nmi,
        } = self;
        clock.save(out);
        save_fitted(pcb.as_ref(), out);
        planar.save(out);
        stolen.save(out);
        held.save(out);
        next_refresh.save(out);
        rescheduled.save(out);
        host_pointer.save(out);
        save_fitted(fpu.as_ref(), out);
        // The line is driven again from the restored 8087 and mask.
        nmi.latched.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.clock.restore(input)?;
        check(CpuClock::new(self.clock.rate()).is_some())?;
        restore_fitted(self.pcb.as_mut(), input)?;
        self.planar.restore(input)?;
        self.stolen.restore(input)?;
        self.held.restore(input)?;
        self.next_refresh.restore(input)?;
        self.rescheduled.restore(input)?;
        self.host_pointer.restore(input)?;
        restore_fitted(self.fpu.as_mut(), input)?;
        self.nmi.latched.restore(input)?;
        check(self.within_reach())?;
        self.map_restored();
        self.planar.memory.replaced();
        self.waits_changed = true;
        Ok(())
    }
}

/// How far from the board's time a restored device may stand: further than
/// any part of a running machine ever falls behind, near enough that
/// catching one up is bounded work.
const REACH: Nanos = NANOS_PER_SECOND;

impl Board {
    /// Whether every device a restore set stands within [`REACH`] of the
    /// planar's time, as a running machine keeps them, and no more bus time
    /// is owed than [`REACH`] of the processor's clock: a device far behind
    /// would be caught up over all the time between.
    fn within_reach(&self) -> bool {
        let planar = &self.planar;
        let now = planar.now;
        let near = |t: Nanos| t.abs_diff(now) <= REACH;
        let owed = self.clock.rate().ticks_at(REACH);
        let pit = |tick: u64| near(Hertz::PIT.time_of(tick));
        let mixed = |sample: u64| {
            sample
                .checked_mul(TICKS_PER_SAMPLE)
                .is_some_and(|tick| near(Hertz::CRYSTAL.time_of(tick)))
        };
        let pcb = self
            .pcb
            .as_ref()
            .zip(self.pcb_clock)
            .is_none_or(|(pcb, rate)| near(rate.time_of(pcb.clocks())) && pcb.stolen() <= owed);
        let sound = &planar.sound;
        let fm = sound.fm.as_ref().is_none_or(|fm| mixed(fm.sampled()));
        let dsp = sound.blaster.as_ref().is_none_or(|blaster| {
            blaster
                .dsp
                .next_event()
                .is_none_or(|at| at.saturating_add(REACH) >= now)
        });
        self.stolen <= owed
            && self.held <= owed
            && pcb
            && near(planar.rendered)
            && pit(planar.pit_tick)
            && pit(planar.refresh_tick)
            && planar.cards.iter().all(|card| near(card.drawn_to()))
            && mixed(sound.mixer.unrendered())
            && fm
            && dsp
    }
}

impl Board {
    /// Shows in the address space and the port decode what the restored
    /// devices map, as each did when the guest programmed it: every card's
    /// window and ports, every expanded-memory window, the 80186's control
    /// block and chip selects; then drives the interrupt lines from them.
    fn map_restored(&mut self) {
        for n in 0..self.planar.cards.len() {
            // A card that found its ports or window taken when the guest
            // moved it found the same then.
            let _ = self.planar.claim_ports(n);
            let _ = self.planar.map_card(n);
        }
        if let Some(expanded) = self.planar.ems.as_ref() {
            for n in 0..ems::WINDOWS {
                expanded.show(n, &mut self.planar.memory);
            }
        }
        let overlay = match self.pcb.as_ref().map(Pcb::space) {
            Some(Space::Memory(base)) => Some(base),
            _ => None,
        };
        self.planar.memory.set_overlay(overlay);
        self.chip_selects_changed();
        self.planar.sync_lines();
        // The line as the restored 8087 and mask drive it, which raised no
        // edge the latch has not kept.
        self.nmi.line = self.nmi_line();
        self.sync_interrupts();
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
