# lemnos-light

`#![no_std]`, allocation-free light rendering: easing curves (linear, ease-in, ease-out,
ease-in-out, sine, cubic Bézier, all fixed point), effects (`Solid`, hard `Blink`, eased
`Breathe` with rate and depth), an `Animator` that fades between looks (colour, frames and
brightness) and returns a frame only when the output changed, and an `Arbiter` that picks
which owner's intent a light shows (`Locate` > `System` > `Alert` > `Test` > `Status` > `App`, then priority,
then recency). `Defaults` carry a board's choices (fade time, easing, status effect,
colours). `lemnosd` drives the board's LEDs with it; firmware can use the same code.
