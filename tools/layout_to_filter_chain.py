#!/usr/bin/env python3
"""layout_to_filter_chain.py LAYOUT.json [--rate HZ] -- PipeWire filter-chain for an AppleHDA speaker layout.

Reads a layout's plist converted to JSON (the layout files ship with macOS's
AppleHDA; they are not part of this project), follows the IntSpeaker SoftwareDSP graph through its
PatchbayInfo, and writes a filter-chain sink: stereo in, four channels out in
davidjo's order (tweeter L, tweeter R, woofer L, woofer R).

Translated: DspEqualization32 bands (types 0 low-pass, 1 high-pass, 4 peak,
5 low shelf, 6 high shelf), DspGainStage, Dsp2To4Splitter,
DspDelay (param 2 in whole ms: 5 ms on the tweeters, which is what lines them
up with BuzzKill's 220 samples on the woofers) and Dsp4ChOutput's per-channel
delay, polarity, mute and gain (0.0833 ms more on the tweeters). Its routing
mode is not: it orders the HDA stream, and the sink keeps davidjo's order.
Without --lv2 the dynamics blocks are passed through: the compressors,
DspLoudness, DspBuzzKill, DspControlFreak, DspThermalSpeakerProtection4ch.
Without a limiter the input is attenuated by the graph's largest boost
(20 Hz to 20 kHz, any output), so the graph itself cannot clip.

With --coeffs, every EQ band that has a row in the TSV (a header row, then
columns path, func as "fN", ch as "0" or "0/1", band, b0, b1, b2, a1, a2,
for --rate; only rows with path "speaker" are read, since the mic's EQ has
function numbers of its own) becomes a bq_raw with those numbers, and the
response is computed from them. Bands without a row fall back to PipeWire's
cookbook bq_* builtins, whose shelves are not the layout's Q-parameterised
ones and differ by up to 21 dB.

With --lv2, the dynamics blocks in ../dsp go into the graph as
LV2 nodes instead of being passed through: DspMozartCompressor
(urn:mbp143-audio:mozart) and DspMozartCompressorDualBand
(urn:mbp143-audio:dualband) on the common path, DspLoudness
(urn:mbp143-audio:loudness) after Mozart, DspBuzzKill
(urn:mbp143-audio:buzzkill) on the woofer branch, DspControlFreak (one
stereo urn:mbp143-audio:controlfreak per branch) and
DspThermalSpeakerProtection4ch (urn:mbp143-audio:thermal,
all four speakers). The sink's volume then goes to the Loudness node instead of
being applied by PipeWire ahead of the graph: capture.volumes hands the channel
volumes to its volume_l and volume_r, so the boost follows the volume and the
volume is applied after Mozart. That stage can never
add level (the boost below the -6 dB offset is at most half the cut), so the
headroom stays as computed. Needs the
bundle installed (`make install` in ..) and PipeWire's LV2 filter-graph
plugin (pipewire-audio on Arch and CachyOS ships it). --response shows the
linear chain only: ControlFreak and the thermal protection are unity below
their knees, but Mozart adds +9 dB input gain and +6..+8.6 dB of curve gain
(about +15.6 dB on a steady tone at -30 dBFS) and the DualBand +6 dB of
make-up on quiet material, which it does not include.
"""
import argparse, cmath, csv, json, math, struct, sys

BQ = {0: 'bq_lowpass', 1: 'bq_highpass', 4: 'bq_peaking', 5: 'bq_lowshelf', 6: 'bq_highshelf'}
LV2 = {'DspMozartCompressor': ('urn:mbp143-audio:mozart', ['l', 'r']),
       'DspMozartCompressorDualBand': ('urn:mbp143-audio:dualband', ['l', 'r']),
       'DspLoudness': ('urn:mbp143-audio:loudness', ['l', 'r']),
       'DspBuzzKill': ('urn:mbp143-audio:buzzkill', ['l', 'r']),
       'DspControlFreak': ('urn:mbp143-audio:controlfreak', ['l', 'r']),
       'DspThermalSpeakerProtection4ch': ('urn:mbp143-audio:thermal',
                                          ['tl', 'tr', 'wl', 'wr'])}
PASS = {'DspMozartCompressor', 'DspMozartCompressorDualBand', 'DspLoudness', 'DspBuzzKill',
        'DspControlFreak', 'DspThermalSpeakerProtection4ch'}


def f32r(x):
    return struct.unpack('<f', struct.pack('<f', x))[0]


def delay_node(name, samples, rate, latency=0):
    # A quarter sample over, so that delay*rate gives the same count whether
    # PipeWire's builtin truncates it or rounds it: 4/44100 written to eight
    # places comes back as 3.99987.
    #
    # latency is what the node reports to PipeWire, in samples: the part of
    # the delay that lines its branch up with the other one. PipeWire adds
    # the reports up along every path and wants the outputs equal, in float,
    # so the value is written to come back as the whole number
    # (220/44100 to nine places times 44100 is exactly 220 in single
    # precision).
    config = {'max-delay': 0.01}
    if latency:
        config['latency'] = f'{latency / rate:.9f}'
    return {'name': name, 'label': 'delay', 'config': config,
            'control': {'Delay (s)': f'{(samples + 0.25) / rate:.9f}'}}


def buzzkill_latency(params, rate):
    # L = rate * ms / 1000 in single precision, rounded half up, the latency
    # in ms being layout param 44; the audio runs through delay lines of
    # L-65 and 64 samples (dsp/src/buzzkill.rs).
    L = int(f32r(f32r(rate * f32r(f32(params.get('44', 5.0)) / 1000.0)) + 0.5))
    return max(L - 65, 0) + 64


def f32(v):
    if isinstance(v, bool) or not isinstance(v, int) or -1024 < v < 1024:
        return v
    return struct.unpack('<f', struct.pack('<i', v if v < 2**31 else v - 2**32))[0]


def coeffs(kind, f, q, gain, rate):
    """RBJ cookbook biquad, normalised to a0 = 1, as PipeWire computes its bq_* nodes."""
    w0 = 2 * math.pi * f / rate
    cw, sw = math.cos(w0), math.sin(w0)
    alpha = sw / (2 * q)
    a = 10 ** (gain / 40)
    if kind == 0:
        b = ((1 - cw) / 2, 1 - cw, (1 - cw) / 2); den = (1 + alpha, -2 * cw, 1 - alpha)
    elif kind == 1:
        b = ((1 + cw) / 2, -(1 + cw), (1 + cw) / 2); den = (1 + alpha, -2 * cw, 1 - alpha)
    elif kind == 4:
        b = (1 + alpha * a, -2 * cw, 1 - alpha * a); den = (1 + alpha / a, -2 * cw, 1 - alpha / a)
    elif kind in (5, 6):
        s = 2 * math.sqrt(a) * alpha
        sign = 1 if kind == 5 else -1
        b = (a * ((a + 1) - sign * (a - 1) * cw + s), sign * 2 * a * ((a - 1) - sign * (a + 1) * cw),
             a * ((a + 1) - sign * (a - 1) * cw - s))
        den = ((a + 1) + sign * (a - 1) * cw + s, -sign * 2 * ((a - 1) + sign * (a + 1) * cw),
               (a + 1) + sign * (a - 1) * cw - s)
    else:
        raise ValueError(kind)
    return [x / den[0] for x in b], [x / den[0] for x in den]


def loudness_gains(volume_db, p):
    """The DspLoudness law: the two shelf gains in dB for a master volume in dB.

    p holds the layout's parameters 10-17 (lowFreq, highFreq, lowSlope,
    highSlope, offset, stop, stopGain, pass). The same law as
    ../dsp/src/loudness.rs.
    """
    k = math.log((10 ** (p['stopGain'] / -10.0) - 1) / (10 ** 0.01 - 1)) / \
        (2 * math.log(p['stop'] / p['pass']))
    if k <= 1.0:
        k, x0 = 1.0, 1.0
    else:
        x0 = p['stop'] / (10 ** (p['stopGain'] / -10.0) - 1) ** (1 / (2 * k))
    if volume_db >= p['offset']:
        return 0.0, 0.0
    scale = 1 / math.sqrt((volume_db / x0) ** (2 * k) + 1) if k > 1.0 else 1.0
    d = p['offset'] - volume_db
    return p['lowSlope'] * d * scale, p['highSlope'] * d * scale


def mag_db(chain, f, rate):
    z = cmath.exp(-2j * math.pi * f / rate)
    h = 1
    for b, a in chain:
        h *= (b[0] + b[1] * z + b[2] * z * z) / (a[0] + a[1] * z + a[2] * z * z)
    return 20 * math.log10(abs(h))


def load(path):
    d = json.load(open(path))
    for pm in d['PathMapRef']:
        if 'IntSpeaker' in pm:
            sp = pm['IntSpeaker']['SignalProcessing'][0]['SoftwareDSP']
            return d['LayoutID'], {int(k[11:]): v for k, v in sp.items() if k.startswith('DspFunction')}
    sys.exit('no IntSpeaker SoftwareDSP in layout')


def load_coeffs(path):
    out = {}
    for r in csv.DictReader(open(path), delimiter='\t'):
        if r['path'] != 'speaker':
            continue
        b = [float(r[k]) for k in ('b0', 'b1', 'b2')]
        a = [1.0, float(r['a1']), float(r['a2'])]
        for ch in r['ch'].split('/'):
            out[(int(r['func'][1:]), int(ch), int(r['band']))] = (b, a)
    return out


def build(funcs, rate, captured=None, volume_db=None, lv2=False):
    """Walk the graph; returns pipewire nodes, links, the 4 output endpoints and per-output biquad chains."""
    nodes, links = [], []
    port = {}      # (func instance, output port) -> (endpoint "node:Out", list of biquads so far, gain)
    inputs = []
    # BuzzKill's latency on the woofers, which the tweeters' delay stage is
    # there to match
    branch_latency = max([buzzkill_latency(f.get('ParameterInfo', {}), rate) for f in funcs.values()
                          if f['FunctionInfo']['DspFuncName'] == 'DspBuzzKill'], default=0)
    for idx in sorted(funcs):
        fn = funcs[idx]
        name = fn['FunctionInfo']['DspFuncName']
        inst = fn['FunctionInfo']['DspFuncInstance']
        params = fn.get('ParameterInfo', {})
        pb = fn.get('PatchbayInfo', {})
        nin = {'Dsp4ChOutput': 4, 'DspThermalSpeakerProtection4ch': 4}.get(name, 2)
        src = []
        for i in range(nin):
            conn = pb.get(f'InputPort{i}')
            if conn is None:
                src.append(None)
                continue
            c = conn[0] if isinstance(conn, list) else conn
            src.append(port[(c['SourceFuncInstance'], c['SourcePortIndex'])])
        if not pb:  # first function: the sink's own inputs
            src = [None, None]
        out = []
        if lv2 and name in LV2:
            uri, sym = LV2[name]
            n = f'f{inst}{name[3].lower()}{name[4:]}'
            nodes.append({'name': n, 'lv2': uri})
            for s, c in zip(src, sym):
                links.append((s[0], f'{n}:in_{c}'))
                out.append((f'{n}:out_{c}', s[1], s[2]))
            src = []
        for ch, s in enumerate(src):
            if name == 'DspEqualization32':
                prev = s
                for flt in sorted(params.get('Filter', []), key=lambda g: g.get('3', 0)):
                    g = {k: f32(v) for k, v in flt.items()}
                    if g.get('2') != ch or g.get('4'):
                        continue
                    n = f'f{inst}c{ch}b{g["3"]}'
                    key = (inst, ch, g['3'])
                    if captured and key in captured:
                        b, a = captured[key]
                        nodes.append({'name': n, 'label': 'bq_raw', 'raw': (rate, b, a)})
                    else:
                        nodes.append({'name': n, 'label': BQ[g['5']],
                                      'control': {'Freq': round(g['6'], 4), 'Q': round(g['7'], 4),
                                                  'Gain': round(g['8'], 4) if g['5'] in (4, 5, 6) else 0.0}})
                        b, a = coeffs(g['5'], g['6'], g['7'], g['8'], rate)
                    if prev is None:
                        inputs.append(f'{n}:In'); prev = (f'{n}:Out', [(b, a)], 1.0)
                    else:
                        links.append((prev[0], f'{n}:In')); prev = (f'{n}:Out', prev[1] + [(b, a)], prev[2])
                out.append(prev)
            elif name == 'DspGainStage':
                gain = f32(params[str(2 + ch)])
                n = f'f{inst}c{ch}gain'
                nodes.append({'name': n, 'label': 'linear', 'control': {'Mult': round(gain, 6), 'Add': 0.0}})
                links.append((s[0], f'{n}:In'))
                out.append((f'{n}:Out', s[1], s[2] * gain))
            elif name == 'DspDelay':
                # param 2: an integer in ms, clamped to 0..100; in samples,
                # rate * ms / 1000 in single precision, rounded half up.
                ms = min(max(int(params['2']), 0), 100)
                samples = int(f32r(f32r(rate * f32r(ms / 1000.0)) + 0.5))
                n = f'f{inst}c{ch}delay'
                # Reported as latency: BuzzKill's, so both branches report the
                # same. The sample or so beyond it, like the output stage's
                # delay, is the offset the layout puts between tweeters and woofers.
                nodes.append(delay_node(n, samples, rate, min(samples, branch_latency)))
                links.append((s[0], f'{n}:In'))
                out.append((f'{n}:Out', s[1], s[2]))
            elif name == 'DspBuzzKill':
                # Passed through, but not without its latency, or the tweeters'
                # 5 ms delay would have nothing to line up with.
                L = buzzkill_latency(params, rate)
                n = f'f{inst}c{ch}delay'
                nodes.append(delay_node(n, L, rate, L))
                links.append((s[0], f'{n}:In'))
                out.append((f'{n}:Out', s[1], s[2]))
            elif name == 'Dsp4ChOutput':
                # params 2-5 delay in ms, 6-9 polarity (bit 0
                # inverts), 10-13 mute, 14-17 gain in dB, per channel.
                p = {k: f32(params.get(str(k), 0)) for k in (2 + ch, 6 + ch, 10 + ch, 14 + ch)}
                samples = int(f32r(f32r(rate * 0.001) * f32r(p[2 + ch]) + 0.5))
                gain = (0.0 if p[10 + ch] else 10 ** (p[14 + ch] / 20)) * (-1 if int(p[6 + ch]) & 1 else 1)
                if samples:
                    n = f'f{inst}c{ch}delay'
                    nodes.append(delay_node(n, samples, rate))
                    links.append((s[0], f'{n}:In'))
                    s = (f'{n}:Out', s[1], s[2])
                if gain != 1.0:
                    n = f'f{inst}c{ch}gain'
                    nodes.append({'name': n, 'label': 'linear', 'control': {'Mult': round(gain, 6), 'Add': 0.0}})
                    links.append((s[0], f'{n}:In'))
                    s = (f'{n}:Out', s[1], s[2] * abs(gain))
                out.append(s)
            elif name == 'DspLoudness' and volume_db is not None:
                q = {n: f32(params[str(i)]) for n, i in
                     (('lowFreq', 10), ('highFreq', 11), ('lowSlope', 12),
                      ('highSlope', 13), ('offset', 14), ('stop', 15),
                      ('stopGain', 16), ('pass', 17))}
                low_db, high_db = loudness_gains(volume_db, q)
                prev = s
                # these shelves are the RBJ cookbook ones with slope S = 1,
                # which is Q = 1/sqrt(2) in the form coeffs() uses (kind 5 low,
                # 6 high), not the EQ's Q-parameterized shelves.
                for tag, kind, freq, db in (('lo', 5, q['lowFreq'], low_db),
                                            ('hi', 6, q['highFreq'], high_db)):
                    n = f'f{inst}c{ch}loud{tag}'
                    b, a = coeffs(kind, freq, 1 / math.sqrt(2), db, rate)
                    nodes.append({'name': n, 'label': 'bq_raw', 'raw': (rate, b, a)})
                    if prev is None:
                        inputs.append(f'{n}:In'); prev = (f'{n}:Out', [(b, a)], 1.0)
                    else:
                        links.append((prev[0], f'{n}:In'))
                        prev = (f'{n}:Out', prev[1] + [(b, a)], prev[2])
                out.append(prev)
            else:
                if name not in PASS | {'Dsp2To4Splitter'}:
                    sys.exit(f'unhandled DSP function {name}')
                out.append(s)
        if name == 'Dsp2To4Splitter':
            out = [out[0], out[1], out[0], out[1]]
        for p, o in enumerate(out):
            port[(inst, p)] = o
        last = out
    return nodes, links, inputs, last


def conf(layout, nodes, links, inputs, outs, headroom_db, rate):
    head = {'name': 'headroom', 'label': 'linear', 'control': {}}
    lines = ['# Generated by layout_to_filter_chain.py from AppleHDA layout %d. Do not edit.' % layout,
             '# Speaker sink for the MacBookPro14,3: stereo in, tweeter L/R and woofer L/R out',
             '# (davidjo snd_hda_macbookpro channel order). %s;' % (
                 'LV2: ' + ', '.join(sorted({n['lv2'].split(':')[-1] for n in nodes if 'lv2' in n}))
                 + ', other dynamics passed through'
                 if any('lv2' in n for n in nodes) else 'Dynamics not yet included'),
             '# input attenuated by %.2f dB so the EQ cannot clip.' % headroom_db,
             'context.modules = [', '  { name = libpipewire-module-filter-chain', '    args = {',
             '      node.description = "MacBook Pro 14,3 Speakers"',
             '      media.name = "MacBook Pro 14,3 Speakers"', '      filter.graph = {', '        nodes = [']
    mult = round(10 ** (-headroom_db / 20), 6)
    for ch in (0, 1):
        lines.append('          { type = builtin name = headroom%d label = linear control = { "Mult" = %s "Add" = 0.0 } }' % (ch, mult))
    for n in nodes:
        if 'lv2' in n:
            lines.append(f'          {{ type = lv2 name = {n["name"]} plugin = "{n["lv2"]}" }}')
            continue
        if 'raw' in n:
            rate, b, a = n['raw']
            c = ' '.join(f'{k} = {v!r}' for k, v in zip(('b0', 'b1', 'b2', 'a0', 'a1', 'a2'), b + a))
            lines.append(f'          {{ type = builtin name = {n["name"]} label = bq_raw config = {{ coefficients = [ {{ rate = {rate} {c} }} ] }} }}')
            continue
        ctl = ' '.join(f'"{k}" = {v}' for k, v in n['control'].items())
        cfg = (' config = { ' + ' '.join(f'"{k}" = {v}' for k, v in n['config'].items()) + ' }') if 'config' in n else ''
        lines.append(f'          {{ type = builtin name = {n["name"]} label = {n["label"]}{cfg} control = {{ {ctl} }} }}')
    lines.append('        ]')
    lines.append('        links = [')
    for ch, i in enumerate(inputs[:2]):
        lines.append(f'          {{ output = "headroom{ch}:Out" input = "{i}" }}')
    for o, i in links:
        lines.append(f'          {{ output = "{o}" input = "{i}" }}')
    lines.append('        ]')
    lines.append('        inputs = [ "headroom0:In" "headroom1:In" ]')
    lines.append('        outputs = [ ' + ' '.join(f'"{o[0]}"' for o in outs) + ' ]')
    loud = [n['name'] for n in nodes if n.get('lv2') == LV2['DspLoudness'][0]]
    if loud:
        # The sink's channel volumes, linear, go to the plugin instead of
        # being applied ahead of the graph (filter-graph.c sync_volume).
        lines.append('        capture.volumes = [')
        for c in ('l', 'r'):
            lines.append(f'          {{ control = "{loud[0]}:volume_{c}" min = 0.0 max = 1.0 scale = linear }}')
        lines.append('        ]')
    lines += ['      }',
              '      capture.props = {', '        node.name = "audio_effect.mbp143-speakers"', '        media.class = Audio/Sink',
              '        audio.channels = 2', '        audio.position = [ FL FR ]',
              '        # the graph is built at the rate it runs at, and the coefficients are for this one',
              f'        node.force-rate = {rate}',
              '        # idle, the plugins are not freed: the thermal protection keeps what it knows',
              '        session.suspend-timeout-seconds = 0', '      }',
              '      playback.props = {', '        node.name = "effect_output.mbp143-speakers"', '        node.passive = true',
              '        audio.channels = 4', '        audio.position = [ FL FR RL RR ]',
              f'        node.force-rate = {rate}', '        session.suspend-timeout-seconds = 0',
              '        # target.object = "<the CS8409 analog-surround-40 sink>"', '      }',
              '    }', '  }', ']']
    return '\n'.join(lines) + '\n'


RAW_SPEAKERS = 'alsa_output.pci-0000_00_1f.3.analog-surround-40'


def wp_graph(layout, nodes, links, inputs, outs, headroom_db, rate):
    """The same graph as conf(), as the JSON file WirePlumber's software-dsp
    create-filter loads (../conf/wireplumber.conf.in): the filter-chain
    args, with the props that make it stand in for the hidden raw sink."""
    def num(v):
        return float(v) if isinstance(v, str) else v

    mult = round(10 ** (-headroom_db / 20), 6)
    out = [{'type': 'builtin', 'name': f'headroom{ch}', 'label': 'linear',
            'control': {'Mult': mult, 'Add': 0.0}} for ch in (0, 1)]
    for n in nodes:
        if 'lv2' in n:
            out.append({'type': 'lv2', 'name': n['name'], 'plugin': n['lv2']})
        elif 'raw' in n:
            r, b, a = n['raw']
            co = dict(zip(('b0', 'b1', 'b2', 'a0', 'a1', 'a2'), list(b) + list(a)))
            out.append({'type': 'builtin', 'name': n['name'], 'label': 'bq_raw',
                        'config': {'coefficients': [dict(rate=r, **co)]}})
        else:
            d = {'type': 'builtin', 'name': n['name'], 'label': n['label']}
            if 'config' in n:
                d['config'] = {k: num(v) for k, v in n['config'].items()}
            d['control'] = {k: num(v) for k, v in n['control'].items()}
            out.append(d)
    graph = {
        'nodes': out,
        'links': [{'output': f'headroom{ch}:Out', 'input': i} for ch, i in enumerate(inputs[:2])]
                 + [{'output': o, 'input': i} for o, i in links],
        'inputs': ['headroom0:In', 'headroom1:In'],
        'outputs': [o[0] for o in outs],
    }
    loud = [n['name'] for n in nodes if n.get('lv2') == LV2['DspLoudness'][0]]
    if loud:
        graph['capture.volumes'] = [{'control': f'{loud[0]}:volume_{c}', 'min': 0.0, 'max': 1.0,
                                     'scale': 'linear'} for c in ('l', 'r')]
    return {
        'node.description': f'MacBook Pro 14,3 Speakers',
        'media.name': 'MacBook Pro 14,3 Speakers',
        'filter.graph': graph,
        'capture.props': {
            'node.name': 'audio_effect.mbp143-speakers',
            'media.class': 'Audio/Sink',
            'audio.channels': 2,
            'audio.position': ['FL', 'FR'],
            # the graph is built at the rate PipeWire's clock runs at, and
            # the coefficients are for this one: without it a 48000 Hz client
            # takes the clock, and the graph with it, to 48000
            'node.force-rate': rate,
            'device.api': 'dsp',
            'node.virtual': False,
            # above the raw sink's 1009: WirePlumber picks a default sink by
            # priority even when the raw one is hidden, so with no stored
            # choice the speakers would play past every block
            'priority.session': 2100,
            'state.default-volume': 0.125,
            'device.icon-name': 'audio-speakers',
            # WirePlumber suspends a node that has been idle for 5 s, and
            # PipeWire then frees the plugins and makes new ones for the next
            # stream: the thermal protection would start from cold speakers
            # after every pause in playback. The sound card's own sink still
            # suspends.
            'session.suspend-timeout-seconds': 0,
        },
        'playback.props': {
            'node.name': 'effect_output.mbp143-speakers',
            'node.description': f'MacBook Pro 14,3 Speakers DSP Out',
            'target.object': RAW_SPEAKERS,
            'node.dont-fallback': True,
            'node.passive': True,
            'audio.channels': 4,
            'audio.position': ['FL', 'FR', 'RL', 'RR'],
            'node.force-rate': rate,
            'session.suspend-timeout-seconds': 0,
        },
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('layout'); ap.add_argument('--rate', type=int, default=44100)
    ap.add_argument('--coeffs', help='TSV of EQ coefficients at --rate: func, ch, band, b0, b1, b2, a1, a2')
    ap.add_argument('--volume-db', type=float,
                    help='bake the DspLoudness shelves in for this master volume in dBFS. '
                         'Off by default: a fixed boost is wrong at any other volume and can '
                         'clip, so this is for one known listening level, '
                         'not for a chain whose volume moves')
    ap.add_argument('--lv2', action='store_true',
                    help='put the blocks ../dsp implements (Mozart, Loudness with the sink volume, '
                         'the DualBand, BuzzKill, ControlFreak, thermal protection) into the graph as LV2 '
                         'nodes instead of passing them through')
    ap.add_argument('--wireplumber', action='store_true',
                    help='write the graph as the JSON file WirePlumber\'s software-dsp '
                         'create-filter loads, for the installed setup (implies --lv2)')
    ap.add_argument('--response', action='store_true', help='print the per-output response instead')
    a = ap.parse_args()
    if a.wireplumber:
        a.lv2 = True
    if a.lv2 and a.volume_db is not None:
        ap.error('--volume-db bakes Loudness in for one volume; with --lv2 it follows the sink volume')
    layout, funcs = load(a.layout)
    nodes, links, inputs, outs = build(funcs, a.rate, load_coeffs(a.coeffs) if a.coeffs else None,
                                       a.volume_db, a.lv2)
    freqs = [20 * 1000 ** (i / 299) for i in range(300)]
    peaks = []
    for name, o in zip(('tweeterL', 'tweeterR', 'wooferL', 'wooferR'), outs):
        resp = [mag_db(o[1], f, a.rate) + 20 * math.log10(o[2]) for f in freqs]
        peaks.append(max(resp))
        if a.response:
            for f, r in zip(freqs, resp):
                print(f'{name}\t{f:.1f}\t{r:.2f}')
    if a.wireplumber:
        json.dump(wp_graph(layout, nodes, links, inputs, outs, max(0.0, max(peaks)), a.rate),
                  sys.stdout, indent=2)
        sys.stdout.write('\n')
    elif not a.response:
        sys.stdout.write(conf(layout, nodes, links, inputs, outs, max(0.0, max(peaks)), a.rate))


main()
