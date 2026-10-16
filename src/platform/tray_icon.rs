//! Tray state stays recognizable without relying on color or a menu-bar title.

const SIZE: usize = 32;
const PIXELS: usize = SIZE * SIZE * 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Active,
    Paused,
    Disabled,
    Attention,
}

pub fn state(health: &str) -> State {
    if health.starts_with("Disabled") {
        State::Disabled
    } else if health.starts_with("Paused")
        || health.starts_with("Secure Input")
        || health.starts_with("Excluded application")
    {
        State::Paused
    } else if health.starts_with("Active") || health.starts_with("Enabled") {
        State::Active
    } else {
        State::Attention
    }
}

#[cfg(target_os = "macos")]
const BASE: &[u8; PIXELS] = include_bytes!("../../assets/tray-template.rgba");
#[cfg(target_os = "windows")]
const BASE: &[u8; PIXELS] = include_bytes!("../../assets/tray-icon.rgba");

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn icon(state: State) -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(
        render(BASE, state, cfg!(target_os = "macos")),
        SIZE as u32,
        SIZE as u32,
    )
    .expect("valid tray icon dimensions")
}

fn render(base: &[u8; PIXELS], state: State, template: bool) -> Vec<u8> {
    let mut rgba = base.to_vec();
    if state == State::Active {
        return rgba;
    }

    // Reserve a clear corner so the badge keeps its shape over the base mark.
    // Template icons use alpha alone; Windows adds a contrasting outer stroke.
    for y in 16..SIZE {
        for x in 16..SIZE {
            let pixel = &mut rgba[(y * SIZE + x) * 4..][..4];
            pixel.fill(0);
            let inside = badge_contains(state, x as i32, y as i32);
            let outline = !template
                && (-1..=1).any(|dy| {
                    (-1..=1).any(|dx| badge_contains(state, x as i32 + dx, y as i32 + dy))
                });
            if inside || outline {
                let shade = if !template && inside { 255 } else { 0 };
                pixel.copy_from_slice(&[shade, shade, shade, 255]);
            }
        }
    }
    rgba
}

fn badge_contains(state: State, x: i32, y: i32) -> bool {
    match state {
        State::Active => false,
        State::Paused => {
            (19..=30).contains(&y) && ((19..=22).contains(&x) || (27..=30).contains(&x))
        }
        State::Disabled => {
            (19..=30).contains(&x)
                && (19..=30).contains(&y)
                && ((x - y).abs() <= 1 || (x + y - 49).abs() <= 1)
        }
        State::Attention => {
            let triangle = (18..=31).contains(&y) && (x - 24).abs() * 2 <= y - 17;
            let exclamation = (23..=25).contains(&x) && ((23..=26).contains(&y) || y == 29);
            triangle && !exclamation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &[u8; PIXELS] = include_bytes!("../../assets/tray-icon.rgba");

    #[test]
    fn status_messages_choose_the_expected_shape() {
        let cases = [
            ("Active in Notepad · Full correction", State::Active),
            ("Enabled in TextEdit · Layout only", State::Active),
            ("Paused · 5 min remaining", State::Paused),
            ("Paused in Notepad", State::Paused),
            ("Paused while editing word rules", State::Paused),
            ("Secure Input — correction resumes", State::Paused),
            (
                "Excluded application — choose Full correction",
                State::Paused,
            ),
            ("Disabled until you enable it", State::Disabled),
            ("Starting keyboard listener…", State::Attention),
            ("Keyboard listener unavailable", State::Attention),
            ("Text focus unavailable", State::Attention),
            ("Cannot identify this application", State::Attention),
            ("Keyboard layout unavailable", State::Attention),
            ("", State::Attention),
        ];
        for (health, expected) in cases {
            assert_eq!(state(health), expected, "{health}");
        }
    }

    #[test]
    fn badges_have_distinct_silhouettes_and_preserve_the_base_mark() {
        let states = [
            State::Active,
            State::Paused,
            State::Disabled,
            State::Attention,
        ];
        for template in [false, true] {
            let images: Vec<_> = states
                .iter()
                .map(|&state| render(BASE, state, template))
                .collect();
            assert_eq!(images[0], BASE.as_slice());
            for image in &images {
                assert_eq!(image.len(), PIXELS);
                assert_eq!(&image[..16 * SIZE * 4], &BASE[..16 * SIZE * 4]);
                assert!(image.chunks_exact(4).any(|pixel| pixel[3] > 0));
            }
            let silhouettes: Vec<Vec<_>> = images
                .iter()
                .map(|image| image.chunks_exact(4).map(|pixel| pixel[3]).collect())
                .collect();
            for (index, silhouette) in silhouettes.iter().enumerate() {
                for other in &silhouettes[index + 1..] {
                    assert_ne!(silhouette, other);
                }
            }
        }
    }

    #[test]
    fn windows_badges_have_a_contrasting_outline() {
        let image = render(BASE, State::Paused, false);
        let pixel = |x, y| &image[(y * SIZE + x) * 4..][..4];
        assert_eq!(pixel(19, 24), &[255, 255, 255, 255]);
        assert_eq!(pixel(18, 24), &[0, 0, 0, 255]);
        assert_eq!(pixel(24, 24), &[0, 0, 0, 0]);
    }
}
