//! Screen fades: the message a server sends to fade a client's view, and the
//! client's list of the fades in progress.
//!
//! `public/shake.h`'s `ScreenFade_t` and `FFADE_*`, and the fade half of
//! `CViewEffects` (`game/client/view_effects.cpp:815-1009`). What a fade
//! finally *does* to the picture is the presenting pass's —
//! [`ViewFade`](crate::materials::post::ViewFade) — because Valve
//! applies it in `engine_post` rather than drawing a quad, and the reason is
//! in that type.
//!
//! The shake and tilt halves of `CViewEffects` are not here: Portal 2's maps
//! place `env_shake`s, and nothing ports them yet.

/// `FFADE_IN` — fade from the colour to the scene. "Just here so we don't
/// pass 0 into the function."
pub const FFADE_IN: u16 = 0x0001;
/// `FFADE_OUT` — fade from the scene to the colour.
pub const FFADE_OUT: u16 = 0x0002;
/// `FFADE_MODULATE` — multiply the scene by the colour rather than blending
/// towards it.
pub const FFADE_MODULATE: u16 = 0x0004;
/// `FFADE_STAYOUT` — hold at the end of the fade until another fade replaces
/// it, ignoring the hold time.
pub const FFADE_STAYOUT: u16 = 0x0008;
/// `FFADE_PURGE` — clear every other fade first.
pub const FFADE_PURGE: u16 = 0x0010;

/// `SCREENFADE_FRACBITS`: a fade's two times are 7.9 fixed point.
const FRACBITS: u32 = 9;

/// `ScreenFade_t` — one fade, as it crosses from server to client.
///
/// **The two times are kept in the wire format**, 16-bit 7.9 fixed point,
/// rather than as the floats they started as. The format is not ours to
/// change: a fade asked for over 0.3 s arrives as 153/512 = 0.2988 s, and the
/// longest a fade can last is 127.998 s. Every shipped `env_fade` is well
/// inside that — the longest is 3 s with a 10 s hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenFade {
    /// Seconds to fade over, 7.9 fixed.
    pub duration: u16,
    /// Seconds to hold at full before the fade is dropped, 7.9 fixed.
    pub hold_time: u16,
    /// `FFADE_*`.
    pub flags: u16,
    /// The colour to fade to or from, and its alpha at full.
    pub color: [u8; 4],
}

impl ScreenFade {
    /// `UTIL_ScreenFadeBuild` (`game/server/util.cpp:1041`).
    pub fn new(color: [u8; 4], fade_time: f32, hold_time: f32, flags: u16) -> ScreenFade {
        ScreenFade {
            duration: fixed_unsigned16(fade_time),
            hold_time: fixed_unsigned16(hold_time),
            flags,
            color,
        }
    }
}

/// `FixedUnsigned16( value, 1 << SCREENFADE_FRACBITS )` (`util.cpp:770`):
/// scaled, truncated towards zero by the `int` assignment, and clamped.
fn fixed_unsigned16(value: f32) -> u16 {
    // `as i32` truncates like C's float-to-int, and saturates where C would
    // be undefined — both ends are clamped straight after anyway.
    ((value * (1 << FRACBITS) as f32) as i32).clamp(0, 0xFFFF) as u16
}

/// Seconds, from 7.9 fixed.
fn seconds(fixed: u16) -> f32 {
    f32::from(fixed) * (1.0 / (1 << FRACBITS) as f32)
}

/// `screenfade_t` — one fade in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fade {
    /// Alpha per second: negative fading out, positive fading in.
    speed: f32,
    /// When the fade reaches full.
    end: f32,
    /// When a fade that has finished is dropped.
    reset: f32,
    color: [u8; 4],
    flags: u16,
}

/// What the fades add up to this frame: `CViewEffects::GetFadeParams`'s
/// answer, which is `SetViewFadeParams`' input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FadeParams {
    /// The average colour of every live fade, and the highest alpha.
    pub color: [u8; 4],
    /// `FFADE_MODULATE` on any live fade.
    pub modulate: bool,
}

/// The fades a client has been sent. `CViewEffects::m_FadeList`.
#[derive(Debug, Default)]
pub struct ViewFades {
    fades: Vec<Fade>,
}

impl ViewFades {
    /// `CViewEffects::Fade` (`view_effects.cpp:815`): starts one, at `now`.
    ///
    /// A fade *out* runs from `now` to `now + duration` and is then held for
    /// the hold time; a fade *in* is held first and then runs. So the same two
    /// numbers mean "go dark, then stay" one way round and "stay dark, then
    /// clear" the other.
    pub fn fade(&mut self, data: &ScreenFade, now: f32) {
        let mut fade = Fade {
            speed: 0.0,
            end: seconds(data.duration),
            reset: seconds(data.hold_time),
            color: data.color,
            flags: data.flags,
        };
        let alpha = f32::from(data.color[3]);
        if data.duration > 0 {
            if data.flags & FFADE_OUT != 0 {
                if fade.end != 0.0 {
                    fade.speed = -alpha / fade.end;
                }
                fade.end += now;
                fade.reset += fade.end;
            } else {
                if fade.end != 0.0 {
                    fade.speed = alpha / fade.end;
                }
                fade.reset += now;
                fade.end += fade.reset;
            }
        }
        // **After** the new fade's times are worked out and **before** it is
        // added — so a purge clears everything but the fade that asked for it.
        if data.flags & FFADE_PURGE != 0 {
            self.clear();
        }
        self.fades.push(fade);
    }

    /// `ClearAllFades`, which `CViewEffects::LevelInit` calls: a level starts
    /// unfaded, whatever the last one ended on.
    pub fn clear(&mut self) {
        self.fades.clear();
    }

    /// `CViewEffects::FadeCalculate` (`view_effects.cpp:864`), at `now`.
    ///
    /// Drops the fades that are over, then combines the rest: the colours
    /// averaged, **in integers** as Valve's `int m_FadeColorRGBA[4]` did, and
    /// the highest alpha. A zero-duration fade has no speed and sits at full
    /// alpha until it is dropped.
    pub fn calculate(&mut self, now: f32) -> FadeParams {
        self.fades.retain_mut(|fade| {
            // "Keep pushing reset time out indefinitely."
            if fade.flags & FFADE_STAYOUT != 0 {
                fade.reset = now + 0.1;
            }
            !(now > fade.reset && now > fade.end)
        });

        let mut rgba = [0i32; 4];
        let mut modulate = false;
        for fade in &self.fades {
            for (sum, channel) in rgba.iter_mut().zip(fade.color).take(3) {
                *sum += i32::from(channel);
            }
            let full = i32::from(fade.color[3]);
            let alpha = if fade.flags & (FFADE_OUT | FFADE_IN) != 0 {
                // `iFadeAlpha = pFade->Speed * ( pFade->End - curtime )` —
                // a float truncated into an `int` before the full alpha is
                // added back, as the C does.
                let mut alpha = (fade.speed * (fade.end - now)) as i32;
                if fade.flags & FFADE_OUT != 0 {
                    alpha += full;
                }
                alpha.min(full).max(0)
            } else {
                full
            };
            rgba[3] = rgba[3].max(alpha);
            if fade.flags & FFADE_MODULATE != 0 {
                modulate = true;
            }
        }
        if !self.fades.is_empty() {
            let count = self.fades.len() as i32;
            for sum in &mut rgba[..3] {
                *sum /= count;
            }
        }
        FadeParams {
            color: rgba.map(|c| c.clamp(0, 255) as u8),
            modulate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [u8; 4] = [0, 0, 0, 255];

    #[test]
    fn a_fade_out_darkens_over_its_duration_and_then_holds() {
        let mut fades = ViewFades::default();
        // 1 s is exact in 7.9 fixed point, so the arithmetic below is too.
        fades.fade(&ScreenFade::new(BLACK, 1.0, 2.0, FFADE_OUT), 10.0);

        assert_eq!(fades.calculate(10.0).color[3], 0);
        // `-255 * 0.5` is -127.5, truncated to -127, plus 255.
        assert_eq!(fades.calculate(10.5).color[3], 128);
        assert_eq!(fades.calculate(11.0).color[3], 255);
        assert_eq!(fades.calculate(12.9).color[3], 255, "held");
        assert_eq!(fades.calculate(13.1).color[3], 0, "dropped after the hold");
    }

    #[test]
    fn a_fade_in_holds_first_and_then_clears() {
        let mut fades = ViewFades::default();
        fades.fade(&ScreenFade::new(BLACK, 1.0, 0.5, FFADE_IN), 0.0);

        // Held at full for half a second: the alpha formula overshoots and is
        // clamped to the fade's own alpha.
        assert_eq!(fades.calculate(0.25).color[3], 255);
        assert_eq!(fades.calculate(1.0).color[3], 127);
        assert_eq!(fades.calculate(1.6).color[3], 0);
    }

    #[test]
    fn stayout_holds_until_something_replaces_it() {
        let mut fades = ViewFades::default();
        // `@transition_from_map`'s exit fade: 0.3 s, stay out.
        fades.fade(&ScreenFade::new(BLACK, 0.3, 0.0, FFADE_OUT | FFADE_STAYOUT), 0.0);
        assert_eq!(fades.calculate(100.0).color[3], 255);

        // A purging fade in replaces it outright.
        fades.fade(&ScreenFade::new(BLACK, 1.0, 0.0, FFADE_IN | FFADE_PURGE), 100.0);
        assert_eq!(fades.calculate(102.0).color[3], 0);
        assert_eq!(fades.fades.len(), 0);
    }

    #[test]
    fn the_times_cross_in_seven_nine_fixed_point() {
        let fade = ScreenFade::new(BLACK, 0.3, 200.0, FFADE_OUT);
        assert_eq!(fade.duration, 153, "0.3 s truncates to 153/512");
        assert_eq!(fade.hold_time, 0xFFFF, "clamped at 127.998 s");
        assert_eq!(ScreenFade::new(BLACK, -1.0, 0.0, 0).duration, 0);
    }

    #[test]
    fn two_fades_average_their_colours_and_take_the_higher_alpha() {
        let mut fades = ViewFades::default();
        fades.fade(&ScreenFade::new([255, 0, 0, 100], 0.0, 1.0, 0), 0.0);
        fades.fade(&ScreenFade::new([0, 0, 255, 200], 0.0, 1.0, FFADE_MODULATE), 0.0);
        let params = fades.calculate(0.5);
        assert_eq!(params.color, [127, 0, 127, 200]);
        assert!(params.modulate);
    }
}
