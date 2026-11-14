-- mbp143-audio-device.lua: MacBookPro14,3 sound card, kept in the state the
-- speaker graph needs (wireplumber.conf.in).
--
-- 1. The profile follows the headphone jack. The four-channel profile, which
--    carries the speakers through the graph, has no headphone port; the stereo
--    profile has both, but drives the speakers without crossover, limiters or
--    thermal protection. So: headphones plugged -> stereo (headphones), else ->
--    four channels. The choice is made in WirePlumber's own select-profile
--    event, ahead of the stored profile, so a stereo profile stored with
--    headphones in (or before the package was installed) is never applied
--    first: switching away from a profile while its nodes are still being
--    created leaves a stale stereo sink on the speakers' PCM, visible to
--    clients and bypassing the graph, and the microphone graph does not
--    come up. A jack change pushes a select-profile event; a profile set by
--    hand against the jack is undone once it has stood a second. Not
--    saved, so the stored choice is left alone.
-- 2. The raw speaker route is held at 0 dB and unmuted. The volume is the
--    graph's Loudness stage, and the raw sink is hidden from clients, so its own
--    route volume is left over: a fresh card comes up at 40 % (-24 dB) and
--    nothing could change it. Its "PCM" control is alsa-lib's softvol, so 1.0
--    passes the samples unchanged.
-- 3. The microphone graph's pre-amp follows the active input port. The
--    headset mic comes out of the card 35 dB too quiet and gets that much
--    gain; the internal mics get no pre-amp. The gain sits in the graph's
--    preAmp controls, so the source volume stays at 100 %. The builtin
--    linear's Mult stops at 10 (20 dB), so each channel has two stages in
--    series, each set to the square root.

cutils = require ("common-utils")
log = Log.open_topic ("s-mbp143-audio")

local SPEAKERS = "output:analog-surround-40+input:analog-stereo"
local HEADPHONES = "output:analog-stereo+input:analog-stereo"
local SPEAKER_ROUTE = "analog-output-speaker"
local HEADPHONE_ROUTE = "analog-output-headphones"
local HEADSET_MIC_ROUTE = "analog-input-mic"
local HEADSET_MIC_DBV = 35.0
local MIC_FILTER = "audio_effect.mbp143-mic"
local MIXER = "Cirrus Logic CS8409/CS42L83"
local PRODUCT = "0xa170"

local micGain = 1.0

function headphonesIn (device)
  for p in device:iterate_params ("EnumRoute") do
    local route = cutils.parseParam (p, "EnumRoute")
    if route and route.name == HEADPHONE_ROUTE then
      return route.available == "yes"
    end
  end
  return false
end

function activeProfile (device)
  local active = nil
  for p in device:iterate_params ("Profile") do
    active = cutils.parseParam (p, "Profile")
  end
  return active
end

function wantedProfile (device)
  local want = headphonesIn (device) and HEADPHONES or SPEAKERS
  for p in device:iterate_params ("EnumProfile") do
    local profile = cutils.parseParam (p, "EnumProfile")
    if profile and profile.name == want and profile.available ~= "no" then
      return profile
    end
  end
  return nil
end

function isOurs (device)
  local props = device.properties
  return props ["alsa.mixer_name"] == MIXER and props ["device.product.id"] == PRODUCT
end

SimpleEventHook {
  name = "device/mbp143-audio-select-profile",
  before = "device/find-stored-profile",
  interests = {
    EventInterest {
      Constraint { "event.type", "=", "select-profile" },
    },
  },
  execute = function (event)
    local device = event:get_subject ()
    if event:get_data ("selected-profile") or not isOurs (device) then
      return
    end
    local profile = wantedProfile (device)
    if profile then
      log:info (device, "profile " .. profile.name)
      event:set_data ("selected-profile", profile)
    end
  end
}:register ()

-- A profile someone sets by hand (pavucontrol) is switched back, but only
-- once it has stood for SETTLE_MS: switching straight back while its nodes
-- are still being created leaves them behind.
local SETTLE_MS = 1000
local lastProfile, profileSince, settling = nil, 0, false

function followJack (device)
  local want = wantedProfile (device)
  local active = activeProfile (device)
  if not (want and active) then
    return
  end
  local now = GLib.get_monotonic_time () // 1000
  if active.name ~= lastProfile then
    lastProfile, profileSince = active.name, now
  end
  if active.name == want.name or settling then
    return
  end
  local wait = profileSince + SETTLE_MS - now
  if wait > 0 then
    settling = true
    Core.timeout_add (wait, function ()
      settling = false
      followJack (device)
      return false
    end)
    return
  end
  source = source or Plugin.find ("standard-event-source")
  source:call ("push-event", "select-profile", device, nil)
end

function holdSpeakerRoute (device)
  for p in device:iterate_params ("Route") do
    local route = cutils.parseParam (p, "Route")
    if route and route.name == SPEAKER_ROUTE and route.props then
      local pr = route.props.properties
      local vols = pr.channelVolumes or {}
      local off = pr.mute == true
      for i = 1, #vols do
        if vols[i] ~= 1.0 then
          off = true
        end
      end
      if off then
        local ones = {}
        for i = 1, math.max (#vols, 1) do
          ones[i] = 1.0
        end
        log:info (device, "holding the raw speaker route at 0 dB")
        device:set_param ("Route", Pod.Object {
          "Spa:Pod:Object:Param:Route", "Route",
          index = route.index,
          device = route.device,
          props = Pod.Object {
            "Spa:Pod:Object:Param:Props", "Route",
            mute = false,
            channelVolumes = Pod.Array ({ "Spa:Float", table.unpack (ones) }),
          },
          save = false,
        })
      end
    end
  end
end

function setMicGain (node)
  local stage = math.sqrt (micGain)
  node:set_param ("Props", Pod.Object {
    "Spa:Pod:Object:Param:Props", "Props",
    params = Pod.Struct {
      "preAmp0a:Mult", stage, "preAmp0b:Mult", stage,
      "preAmp1a:Mult", stage, "preAmp1b:Mult", stage,
    },
  })
end

micOm = ObjectManager {
  Interest {
    type = "node",
    Constraint { "node.name", "equals", MIC_FILTER, type = "pw-global" },
  }
}

micOm:connect ("object-added", function (om, node)
  setMicGain (node)
end)

function followMicRoute (device)
  local gain = 1.0
  for p in device:iterate_params ("Route") do
    local route = cutils.parseParam (p, "Route")
    if route and route.direction == "Input" and route.name == HEADSET_MIC_ROUTE then
      gain = 10.0 ^ (HEADSET_MIC_DBV / 20.0)
    end
  end
  if gain ~= micGain then
    micGain = gain
    log:info (device, string.format ("mic pre-amp %.2f", micGain))
    for node in micOm:iterate () do
      setMicGain (node)
    end
  end
end

function update (device)
  followJack (device)
  followMicRoute (device)
  local active = activeProfile (device)
  if active and active.name == SPEAKERS then
    holdSpeakerRoute (device)
  end
end

local seen = {}

om = ObjectManager {
  Interest {
    type = "device",
    Constraint { "alsa.mixer_name", "equals", MIXER, type = "pw" },
    Constraint { "device.product.id", "equals", PRODUCT, type = "pw" },
  }
}

om:connect ("objects-changed", function (om)
  local now = {}
  for device in om:iterate () do
    local id = device["bound-id"]
    if not seen[id] then
      device:connect ("params-changed", function (d, id)
        if id == "EnumRoute" or id == "Route" or id == "Profile" or id == "EnumProfile" then
          update (d)
        end
      end)
      update (device)
    end
    now[id] = true
  end
  seen = now
end)

micOm:activate ()
om:activate ()
