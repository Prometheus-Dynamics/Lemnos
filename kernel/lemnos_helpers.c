// SPDX-License-Identifier: GPL-2.0
/*
 * The Lemnos module's only C: one-line shims for static inline kernel APIs with no exported symbol and
 * no rust_helper_* (7.3.3, rust/exports.c). Each is one line, commented with the API it wraps and why
 * there is no Rust path. scripts/check-kernel-helpers.py checks that this file holds nothing else and
 * stays within its size cap.
 */
#include <linux/device/bus.h>
#include <linux/pwm.h>

/* The Rust side declares the same two functions (lemnos_pwm_rs.rs); these are their C prototypes. */
struct device *lemnos_bus_find_device_by_name(const struct bus_type *bus, const char *name);
void lemnos_pwm_get_state(const struct pwm_device *pwm, struct pwm_state *state);

/* bus_find_device_by_name: static inline (include/linux/device/bus.h), no export; the Rust side calls it through this shim. */
struct device *lemnos_bus_find_device_by_name(const struct bus_type *bus, const char *name) { return bus_find_device_by_name(bus, NULL, name); }
/* pwm_get_state: static inline (include/linux/pwm.h:129), reads the device's cached state; no export, no rust_helper. */
void lemnos_pwm_get_state(const struct pwm_device *pwm, struct pwm_state *state) { pwm_get_state(pwm, state); }
