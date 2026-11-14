# mbp143-audio

Audio DSP for the 15" MacBook Pro of 2017 (Generation 14, Variant 3, "MacBookPro14,3").

It runs on Linux, as two PipeWire filter graphs that WirePlumber loads for
the built-in sound card: one for the speakers, one for the microphones.

The machine has four speakers, two tweeters and two woofers, each on its own
amplifier. A plain stereo stream is not what they need: nothing splits it
into highs and lows, nothing corrects for small speakers in a flat case, and
nothing keeps a loud stream from overheating them. The speaker graph does
that:

- an EQ, and a crossover that gives the tweeters the highs and the woofers
  the lows
- Mozart and the DualBand, compressors that lift quiet material
- Loudness, which adds bass and treble as the volume goes down, and applies
  the volume itself
- BuzzKill, which notches out what makes the case rattle
- ControlFreak, a limiter on each pair of speakers
- the thermal protection, which estimates how hot each speaker is and turns
  it down before it overheats

The headset microphone comes out of the sound card about 35 dB too quiet to
use. The microphone graph raises it by that much and leaves the internal
microphones as they are.

Headphones need no processing and get none.

## Supported hardware

The MacBookPro14,3 only (15", 2017, Touch Bar):

    $ cat /sys/class/dmi/id/product_name
    MacBookPro14,3

Everything in the graphs is this model's tuning: the EQ is for its speakers
in its case, the thermal protection for the heat its speakers can take. The
package looks for this model's sound card (Cirrus CS8409, subsystem
`106b3900`) and does nothing on any other machine. Other MacBooks with a
CS8409 have other speakers; this graph would be wrong for them.

## Requirements

- The kernel driver for the sound card,
  [snd_hda_macbookpro](https://github.com/davidjo/snd_hda_macbookpro) by
  davidjo. It is not part of the Linux kernel. Until two fixes are merged
  there, install it from the fork at
  [github.com/dnlbtz/snd_hda_macbookpro](https://github.com/dnlbtz/snd_hda_macbookpro),
  branch `offlineDkmsDeb`, which has packaging for Arch and Ubuntu. Without
  the fixes the speakers stay silent after a hibernation and the headset
  microphone does not work.
- PipeWire 1.4 or later with its LV2 plugin (`pipewire-audio` on Arch and
  CachyOS, `libspa-0.2-modules-extra` on Ubuntu) and WirePlumber 0.5.8 or
  later.

Tested on a MacBookPro14,3 with CachyOS, PipeWire 1.6.10 and WirePlumber
0.5.18. The Ubuntu package is built on 26.04 and its graph loads in 26.04's
PipeWire 1.6.2, but it has not been tested on a 14,3 under Ubuntu yet.

## Install

Install the kernel driver first. Then download the package from the
[releases](https://github.com/dnlbtz/mbp143-audio/releases) and install it:

    sudo pacman -U mbp143-audio-*.pkg.tar.zst      # Arch, CachyOS
    sudo apt install ./mbp143-audio_*.deb          # Ubuntu 26.04

Then restart the audio services, or log out and in:

    systemctl --user restart pipewire pipewire-pulse wireplumber

If you already correct these speakers yourself (an EasyEffects preset, a
filter-chain of your own), switch that off: it would be applied on top of
this graph.

## Uninstall

    sudo pacman -R mbp143-audio                    # Arch, CachyOS
    sudo apt remove mbp143-audio                   # Ubuntu 26.04

Then restart the audio services, or log out and in:

    systemctl --user restart pipewire pipewire-pulse wireplumber

The sound card's raw four-channel sink is visible again afterwards, and
what is played to it reaches the speakers without any protection. Check the
volume before playing something.

## Check that it works

The desktop's sound settings show one output, "MacBook Pro 14,3 Speakers",
and one input, "MacBook Pro 14,3 Microphone", and both are the default.

Play music and turn the volume down: it should get quieter without getting
thin.

`wpctl status` lists both graphs under Filters, each with the node programs
use (marked `*`) and the node that connects it to the sound card:

     ├─ Filters:
     │    - filter-chain-...
     │  *   65. audio_effect.mbp143-speakers        [Audio/Sink]
     │      66. effect_output.mbp143-speakers       [Stream/Output/Audio]
     │    - filter-chain-...
     │      70. audio_effect.mbp143-mic             [Stream/Input/Audio]
     │  *   71. effect_output.mbp143-mic            [Audio/Source]

## What changes on the machine

- The sound card's raw sink, four channels that go straight to the four
  amplifiers, is hidden, and the speaker graph stands in for it: programs
  do not get the raw sink in their list of outputs, and no stream is
  connected to it for them. Anything played to the raw sink directly would
  reach the speakers around the crossover, the limiters and the thermal
  protection. A program that makes its own connections (a patchbay,
  `pw-link`) can still do that, because the raw sink's ports stay visible.
  If the graph fails to load there is no speaker output at all, rather than
  unprotected output.
- A program that opens the sound card itself through ALSA (`aplay -D hw:0`,
  a music program set to its ALSA backend) goes around PipeWire and so
  around the graph too, at full level: the card has no volume of its own.
  It can do that whenever nothing plays through PipeWire, and this package
  cannot prevent it. Set such programs to PipeWire, or to JACK with
  `pipewire-jack` installed.
- The volume you set is applied inside the graph, by Loudness, because the
  bass and treble it adds depend on the volume. The raw sink stays at 0 dB.
- The graph runs at 44100 Hz, the rate its filters are computed for and the
  only one the speakers take. While something plays through the speakers
  PipeWire's clock is held there, and a 48000 Hz stream is resampled on its
  way in. Other devices keep their own rates.
- The sound card's profile follows the headphone jack: four channels through
  the speaker graph with nothing plugged in, plain stereo with headphones.
- The microphone graph becomes the default input. The sound card's raw
  source stays visible, because that is where a desktop offers the choice
  between the internal and the headset microphone. Recording from the raw
  source directly gives the headset microphone 35 dB too quiet.

## The graphs

### Speakers

PipeWire shows two nodes:

- `audio_effect.mbp143-speakers`, "MacBook Pro 14,3 Speakers": the stereo
  sink that programs play to. Its volume is the speaker volume.
- `effect_output.mbp143-speakers`: the graph's output, a four-channel stream
  into the hidden raw sink. `FL` and `FR` are the left and right tweeter,
  `RL` and `RR` the left and right woofer.

Between the two the signal passes these stages (`graphs/speakers.json`):

    stereo in
        |
       f0   high-pass
       f1   Mozart
       f2   Loudness          <- the sink's volume
       f3   treble shelf
       f4   DualBand
       f5   EQ
       f6   gain
        |
        +-------------------------+
        |                         |
       f8   tweeter filters      f9   woofer filters
       f10  delay                f11  BuzzKill
       f12  ControlFreak         f13  ControlFreak
        |                         |
        +-------------------------+
        |
       f14  thermal protection, all four channels
       f15  delay, tweeters only
        |
    FL FR tweeters, RL RR woofers

A node is named `f<stage>`, and where a stage is a chain of filters,
`f<stage>c<channel>b<band>`: `f5c1b3` is the fourth EQ band of the right
channel. Mozart, DualBand, Loudness, BuzzKill and ControlFreak are the
names these blocks have in Apple's audio layout for this machine. Those
five and the thermal protection are LV2 plugins written in Rust (`dsp/`,
`lv2/`); everything else is built into PipeWire. Their settings for these
speakers, the speakers' resistances, thermal time constants and
temperature limit among them, come from the same layout as the graph
(`Config::mbp143()` in each block).

| node | what it is | what it does |
|---|---|---|
| `headroom` | gain, 0 dB | The graph's input. A place to attenuate ahead of the EQ; this graph does not need it, so it passes the signal unchanged. |
| `f0` | high-pass, 88 Hz, 12 dB per octave | Removes what the woofers cannot reproduce anyway and would only heat them. |
| `f1` Mozart | upward compressor | Raises quiet material, by up to about 16 dB, so quiet passages stay audible on small speakers. 74 samples of latency. |
| `f2` Loudness | volume and two shelves | Applies the sink's volume. Below -6 dB it adds bass under 300 Hz and a little treble above 2 kHz, more the lower the volume, because the ear loses both first at low level. |
| `f3` | high shelf, +4 dB, centred at 2.5 kHz | Treble lift. |
| `f4` DualBand | two-band compressor, split at 400 Hz | Compresses bass and the rest separately, so a loud bass note does not pull the voice down with it. |
| `f5` | EQ, 16 bands per channel | The correction for these speakers in this case. It leaves the deep bass almost alone (less than 1 dB below 60 Hz), takes 100 to 200 Hz down by 2 to 7 dB, and everything above 300 Hz by 10 dB and more. |
| `f6` | gain, +1.5 dB | Level trim after the EQ. |
| `f8` | high-pass near 1 kHz, 24 dB per octave, and six EQ bands | The tweeters' half of the crossover and their own correction. |
| `f9` | low-pass near 1 kHz, 24 dB per octave, and five EQ bands | The woofers' half of the crossover and their own correction. |
| `f10` | delay, 5 ms | Keeps the tweeters in step with the woofers, which BuzzKill delays by as much. |
| `f11` BuzzKill | adaptive notch filters on the woofers | Watches for a strong sustained low tone, the kind that makes the case buzz, and puts a narrow notch on it, up to 50 at once. How much it does depends on the volume: on a film soundtrack at full volume it was at work about half the time and for moments took out up to half of the woofers' energy; 20 dB lower it never took out more than 2 %. 220 samples of latency. |
| `f12`, `f13` ControlFreak | lookahead limiter, one for the tweeters, one for the woofers | Catches peaks before they reach the amplifier, half a millisecond ahead. Left and right of a pair get the same gain, so the stereo image does not shift. |
| `f14` | thermal protection | Estimates each speaker's temperature from the power it has been given. When one gets close to its limit, its pair is turned down within a second and comes back over 20 seconds. Only reached by long, very loud playback. |
| `f15` | delay, 4 samples, tweeters only | Time alignment of the tweeters to the woofers. |

From sink to sound card the graph takes 316 samples, about 7 ms.

### Microphone

PipeWire shows two nodes for this graph as well:

- `audio_effect.mbp143-mic`: the graph's input, a stream from the sound
  card's raw source.
- `effect_output.mbp143-mic`, "MacBook Pro 14,3 Microphone": the source that
  programs record from.

Between the two, per channel (`graphs/mic.json`):

| node | what it is | what it does |
|---|---|---|
| `preAmp0a`, `preAmp0b` (left), `preAmp1a`, `preAmp1b` (right) | two gains in series | Together 35 dB while the headset microphone is the active input, 0 dB for the internal microphones. Two stages because one PipeWire gain stops at 20 dB. |

The gain is set by `conf/mbp143-audio-device.lua`, a WirePlumber script
that follows the sound card's active input. The same script switches the
sound card's profile with the headphone jack and holds the raw sink at
0 dB.

## Troubleshooting

### No "MacBook Pro 14,3 Speakers" in the output list, no sound

The speaker graph did not load. `journalctl --user -b -u wireplumber` says
why. The usual cause is PipeWire's LV2 plugin missing
(`libspa-0.2-modules-extra` on Ubuntu). `lv2ls | grep mbp143` should print
six lines (`lv2ls` is in `lilv-tools` on Arch and CachyOS, `lilv-utils` on
Ubuntu); if it prints nothing, the package is not installed completely.

### No sound card at all

That is the kernel driver, not this package.
`grep -l CS8409 /proc/asound/card*/codec*` should name a file.

### Silent after a hibernation

The kernel driver did not switch the amplifiers on again. davidjo's driver
does not do that yet. Install the driver from
[github.com/dnlbtz/snd_hda_macbookpro](https://github.com/dnlbtz/snd_hda_macbookpro),
branch `offlineDkmsDeb`, which does.

### The headset microphone is very quiet

The program records from the sound card's raw source. Select "MacBook Pro
14,3 Microphone" as its input.

### A browser stops playing when headphones go in or come out

The switch between speakers and headphones removes the sink the browser was
playing to, and browsers pause instead of moving to the new one. Press
play. Players that follow the default output just carry on.

### The speakers get quieter during long, very loud playback

That is the thermal protection. The volume comes back by itself.

### `graph has unaligned latency min:0.000000 max:220.000000` in WirePlumber's journal

Harmless. It appears once each time WirePlumber starts (at login, and
after a restart of the sound services), when the speakers first play.
PipeWire adds up the latency each plugin reports along the tweeter and the
woofer branch, and it reads the plugins' reports before they have run for
the first time. From the second start on the branches agree, at 316
samples, and the line does not come back until WirePlumber starts again.

## Build from source

Needs Rust (`cargo`); the first build fetches the `lv2` crate.

    make && make check
    sudo make install PREFIX=/usr

`sudo make uninstall PREFIX=/usr` removes it again. The release packages
are built by `makepkg` in `packaging/arch` and by
`packaging/debian/buildDeb`.

## In this repository

- `dsp/`: the six plugins' signal processing in Rust, no dependencies
  (`dsp/README.md`). `make check` runs its tests.
- `lv2/`: the six as LV2 plugins in one bundle (`lv2/README.md`).
- `graphs/`: `speakers.json` and `mic.json`, the two graphs as WirePlumber
  loads them.
- `conf/`: the WirePlumber rules and the script that go with the graphs.
- `tools/layout_to_filter_chain.py`: generates `graphs/speakers.json` from
  an AppleHDA layout converted to JSON (`tools/README.md`). The layout is
  not part of this repository and nothing at install or run time needs
  macOS: the generated graph is what gets installed. The tool is for
  regenerating it from a layout you dump yourself.
- `packaging/`: the Arch PKGBUILD and the Debian package builder.

## Credits

[davidjo](https://github.com/davidjo/snd_hda_macbookpro) wrote the driver
that makes these speakers work on Linux at all.
[asahi-audio](https://github.com/AsahiLinux/asahi-audio), for Apple Silicon
Macs, is where the way the graph is installed comes from: a WirePlumber
`node.software-dsp` rule loads it in front of the sound card's sink and
hides that sink. The processing differs: asahi-audio has a tuning of its
own, this package does what macOS does on the MacBookPro14,3.

## Licence

MIT, see `LICENSE`.
