# Timewarp input processing

Timewarp offers **Process Previous Effects**, initially off. With it off, Timewarp samples
the layer's unprocessed source, preserving the established behavior of existing projects.
With it on, Timewarp evaluates the effects above it at each requested source time before
retiming. The renderer excludes Timewarp itself and all later effects from those nested
evaluations. Effects below Timewarp then run once on its result.

For example, a red solid with a blue Fill followed by Timewarp at 100% remains red in the
default source mode. Enabling Process Previous Effects retains the blue Fill. Adding a
50% green Fill below Timewarp blends that result once, rather than processing it again at
each source sample. The setting applies to Whole, Mix and Motion methods. Pixel Motion
Blur already samples preceding effects and retains its existing behavior.

Existing project instances receive the default through the normal effect-parameter
migration. The control-channel property is `processPreviousEffects` on
`ec.time.timewarp`; its value must be a boolean. Invalid values return an effect error
before source reads.

This is an explicit EffectCraft option. It has not been compared with After Effects and
does not establish compatibility with a commercial retiming plug-in. Expensive preceding
effects can increase work because they are evaluated at the source times needed by the
selected retiming method. No speed improvement is claimed for enabling the option.

Synthetic regressions cover missing/default/enabled values, invalid types, all three
methods, CPU rendering and a GPU-enabled mixed effect stack. A non-idempotent later Fill
detects accidental duplicate processing. Production build validation remains required.
