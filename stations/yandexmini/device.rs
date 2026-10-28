#[path = "hardware.rs"]
mod hardware;
#[path = "microphone.rs"]
mod microphone;

use luna::{LedTopology, StationSpec};

const CAPABILITIES: &[&str] = &[
    "led.ring.rgb",
    "led.ring.6",
    "led.pattern.v1",
    "microphone.capture",
];

pub fn spec() -> StationSpec {
    StationSpec {
        model: "yandexmini",
        led_profile: "mini1",
        led_count: 6,
        led_topology: LedTopology {
            ring_count: 6,
            ..LedTopology::default()
        },
        color_order: luna::ColorOrder::Bgr,
        create_led_backend: hardware::create_led_backend,
        create_microphone_backend: microphone::create_microphone_backend,
        create_clock_backend: || None,
        capabilities: CAPABILITIES,
        input_actions: &[],
        input_gestures: &[],
    }
}
