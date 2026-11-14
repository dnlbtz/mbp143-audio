# lv2

The six blocks of `../dsp` as LV2 plugins, in one bundle, `mbp143-audio.lv2`:
one binary (`mbp143-audio.so`, from this crate), `manifest.ttl`, and a `.ttl`
per plugin that is the reference for its ports. Built on the `lv2` crate.

| plugin | where | ports | latency |
|---|---|---|---|
| `urn:mbp143-audio:thermal` | last, over all four | 4 in, 4 out, gain and temperature per speaker | 0 |
| `urn:mbp143-audio:controlfreak` | per branch | stereo, bypass, the lowest gain of the last run, latency | 22 |
| `urn:mbp143-audio:mozart` | common path, first | stereo, bypass, gain dB, latency | 74 |
| `urn:mbp143-audio:dualband` | common path | stereo, bypass, both bands' gains dB | 0 |
| `urn:mbp143-audio:buzzkill` | woofers | stereo, bypass, notches running, latency | 220 |
| `urn:mbp143-audio:loudness` | common path, after Mozart | stereo, `volume_l`, `volume_r` (linear), bypass, both shelves dB | 0 |

Every bypass keeps the plugin's latency, and the core keeps running under it,
so switching costs no jump: ControlFreak runs a second core configured as
bypass, Mozart and BuzzKill a delay line of their latency, Loudness a second
core with both slopes at 0 so the volume stays applied. The thermal protection
has no bypass: it is what keeps the speakers from overheating, so it cannot be
switched off. The cores get the audio
in chunks of 512 frames, which fixes where the blocks' decimation phases
fall.

Every plugin takes an input sample that is not a number as silence and clips
the rest at 100 (40 dB over full scale), the thermal protection at full
scale, so its output is finite and bounded whatever a program sends. A
sample rate outside 8000 to 192000 Hz is refused when the plugin is loaded.

The volume ports are where the sink's volume goes:
`../tools/layout_to_filter_chain.py --lv2` points the graph's `capture.volumes` at
them, so PipeWire passes the sink's channel volumes in (filter-graph's
`sync_volume`, 1.6.8: `min + (max - min) * v`, linear for these ports) and
sets its own soft volume to 1, which moves the volume into the chain, after
Mozart. LV2 shows a plugin its control ports only in `run`, so the
Loudness plugin takes the volume it starts at, without a glide, on the first
run after `activate`.

BuzzKill sits only on the woofers; the tweeter branch is lined up with it by
a delay in the graph: 5 ms (221 samples) plus 4 samples at the output, so the
tweeters trail the woofers by 5 samples.

PipeWire's filter-graph gives every output port a buffer of its own; the
plugins still copy their input before writing, so an in-place host gets the
right result too.

`lv2ls` lists the six and `lv2bench` runs each.
