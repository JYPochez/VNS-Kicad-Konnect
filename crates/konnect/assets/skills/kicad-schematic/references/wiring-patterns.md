# Common Wiring Patterns

## Pattern 1: Decoupling Capacitor
```
        +3V3 (power symbol)
         |
    ┌────┤
    │    C1 100nF
    │    │
    │    GND (power symbol)
    │
    U1 VCC pin
```
**Tools**: `add_schematic_component` (cap) → `add_power_symbol` (+3V3 above cap) → `add_power_symbol` (GND below cap) → `connect_pins` (cap pin 1 to IC VCC)

## Pattern 2: Pull-up Resistor
```
    +3V3
     |
     R1 4.7k
     |
     ├──── net label "SDA"
     |
    IC pin
```
**Tools**: `add_schematic_component` (R, value 4.7k) → `add_power_symbol` (+3V3) → `connect_to_net` (resistor pin 2, net "SDA", direction down)

## Pattern 3: Voltage Divider
```
    VIN ──── R1 ──┬── R2 ──── GND
                  |
              net label "FB"
```
**Tools**: Place R1 and R2 → `connect_pins` (R1 pin 2 to R2 pin 1) → `add_schematic_net_label` at junction → `connect_to_net` on R1 pin 1 (input) → `add_power_symbol` GND on R2 pin 2

## Pattern 4: LED with Current Limiting Resistor
```
    GPIO_OUT ──── R1 330Ω ──── D1 LED ──── GND
```
**Tools**: Place R1 (330) and D1 (LED) → `connect_pins` (R1 pin 2 to D1 anode/pin 1) → `connect_to_net` (R1 pin 1, net "GPIO_OUT") → `add_power_symbol` (GND on D1 cathode/pin 2)

## Pattern 5: Crystal Oscillator
```
         ┌── C1 ──┐
    OSC_IN ──┤     ├── GND
         │  XTAL  │
    OSC_OUT ─┤     ├── GND
         └── C2 ──┘
```
**Tools**: Place crystal + 2 load caps → `connect_pins` (XTAL pin 1 to C1 pin 1) → `connect_pins` (XTAL pin 2 to C2 pin 1) → `add_power_symbol` GND on C1 pin 2 and C2 pin 2 → `connect_to_net` (XTAL pin 1, "OSC_IN") → `connect_to_net` (XTAL pin 2, "OSC_OUT")

## Pattern 6: USB Type-C Power Sink (5V only)
```
    VBUS ────────── +5V
    CC1 ──── R 5.1k ──── GND
    CC2 ──── R 5.1k ──── GND
    GND ─────────── GND
    D+ ──────────── USB_DP
    D- ──────────── USB_DM
```
**Tools**: Use `search_templates("usb_c_5v_sink")` first — the templates toolset has this pre-built.

## Wiring Decision Guide

| Scenario | Tool | Why |
|----------|------|-----|
| Two specific pins on two components | `connect_pins` | Auto-routes, knows pin coordinates |
| Pin to a named net (signal bus) | `connect_to_net` | Adds stub + label, clean |
| Pin to power rail | `add_power_symbol` | Creates net automatically |
| Multiple pins to same net | `batch_connect_to_net` | Single atomic write |
| Two points already known by coordinates | `add_schematic_connection` | Auto H+V routing |
| Simple horizontal/vertical wire | `add_wire` | Manual, use sparingly |

## Net Label Types

| Type | Scope | When to use |
|------|-------|-------------|
| Net label (`net_label`) | Single sheet | Local signals within one schematic sheet |
| Global label (`global_label`) | All sheets | Signals shared across hierarchical sheets |
| Hierarchical label (`hierarchical_label`) | Sheet boundary | Interface pins on hierarchical sheet symbols |
| Power symbol | Global | Power rails (+3V3, GND, VCC) |

## Spacing Guidelines

- Components: minimum 5.08mm (4 grid units) between component bodies
- Labels: place at wire endpoints, not floating in space
- Power symbols: directly on component power pins when possible
- Junctions: added automatically by Konnect at T-intersections
