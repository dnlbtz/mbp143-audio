# tools

`layout_to_filter_chain.py` generates `../graphs/speakers.json`, the speaker
graph. Python, standard library only.

Nothing at build, install or run time uses it: the generated graph is
committed. The tool is for regenerating the graph. It needs two inputs that
are not part of this repository: the AppleHDA layout for the machine,
converted from its plist to JSON, and the EQ bands' coefficients at
44100 Hz as a TSV (columns in the script's header).

    ./layout_to_filter_chain.py layout57.json --coeffs eq-44100.tsv --wireplumber > ../graphs/speakers.json

What it does on the way:

- The six plugins go into the graph as LV2 nodes: Mozart, Loudness and the
  DualBand on the common path, BuzzKill on the woofer branch, ControlFreak
  per branch and the thermal protection over all four channels.
- Loudness carries the sink's volume: the graph gets `capture.volumes`
  pointing at its `volume_l` and `volume_r`, so PipeWire hands the channel
  volumes to the plugin and does not apply them ahead of the graph.
- The branch delays: the layout's delay stage is whole milliseconds (5 ms,
  221 samples at 44100 Hz) and its output stage delays the tweeters by 4
  samples more, so the tweeters end up 5 samples behind the woofers, which
  BuzzKill delays by 220. Delays are written as (n + 0.25) / rate seconds:
  PipeWire turns delay times rate into a sample count, and a rounded
  4/44100 came back as 3.99987. The 5 ms delay reports BuzzKill's 220
  samples as its latency, so PipeWire, which adds up what the nodes report
  along every path, finds both branches at the same latency.
- The rate: both of the graph's streams carry `node.force-rate`. PipeWire
  builds a filter graph at whatever rate it happens to run at, and the
  coefficients are for 44100 Hz only, so the graph holds the rate there.
- The properties that let the graph stand in for the hidden raw sink
  (`../conf/wireplumber.conf.in`).

Other options, for looking at the graph and not for the installed one:

- Without `--wireplumber` the same graph is written as a configuration file
  for PipeWire's filter-chain module. `--lv2` puts the plugins into that
  form too; without it they are left out, and BuzzKill's place is taken by
  a plain 220-sample delay so the branches still line up.
- `--volume-db G`, only without the plugins: fixes the Loudness shelves for
  one volume G in dBFS. A fixed boost is only right at that volume (at
  -36 dB it is +10.56 dB below 300 Hz), so turning the volume up afterwards
  can clip.
- `--response` prints the frequency response per output, filters only.
- A band without a row in the `--coeffs` file falls back to PipeWire's
  cookbook biquads.
