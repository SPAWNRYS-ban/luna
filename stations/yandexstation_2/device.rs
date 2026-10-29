#[path = "hardware.rs"]
mod hardware;

use luna::{LedTopology, StationSpec};

// Station 2 has no sysfs LED ring. Its 89 LEDs are an XRGB framebuffer, so
// the framebuffer backend is the station-specific capability implementation.
const CAPABILITIES: &[&str] = &[];

pub fn spec() -> StationSpec {
    StationSpec {
        model: "yandexstation_2",
        led_profile: "station2",
        led_count: 89,
        led_topology: LedTopology {
            ring_count: 89,
            ..LedTopology::default()
        },
        color_order: luna::ColorOrder::Rgb,
        create_led_backend: hardware::create_led_backend,
        create_microphone_backend: || None,
        create_clock_backend: || None,
        capabilities: CAPABILITIES,
        input_actions: &[],
        input_gestures: &[],
        gpio_inputs: &[],
    }
}
