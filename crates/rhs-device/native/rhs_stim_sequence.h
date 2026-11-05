#pragma once

#include "rhs_scan_bridge.h"
#include "abstractrhxcontroller.h"

inline int rhs_manual_trigger_source(int trigger)
{
    // Intan source indices: digital 0..15, analog 16..23, manual F1..F8 24..31.
    return 24 + trigger;
}

inline int routed_rhs_stim_mask(const RhsStimConfig& config, int stream, int default_stream)
{
    bool routed = false;
    for (int mask : config.stream_masks) routed = routed || mask != 0;
    return routed ? config.stream_masks[stream] : (stream == default_stream ?
        (config.channel_mask != 0 ? config.channel_mask : (1 << config.channel)) : 0);
}

template <typename Controller, typename Streams>
void enable_rhs_stim_routes(Controller& controller, const Streams& streams, int default_stream,
                            const RhsStimConfig& config)
{
    controller.setManualStimTrigger(config.trigger, false);
    for (int stream : streams) {
        const int mask = routed_rhs_stim_mask(config, stream, default_stream);
        for (int channel = 0; channel < 16; ++channel) {
            controller.configureStimTrigger(stream, channel, rhs_manual_trigger_source(config.trigger),
                (mask & (1 << channel)) != 0, true, false);
        }
    }
}

template <typename Controller>
void program_rhs_stim_sequence(Controller& controller, int stream, int channel,
                               const RhsStimConfig& config)
{
    const int never = 65535;
    const bool interphase = config.interphase_steps > 0;
    const int second_phase_start = config.phase_steps + config.interphase_steps;
    const int end_stim = second_phase_start + config.phase_steps;
    const int end_sequence = config.period_steps * config.pulses + 255;

    controller.configureStimTrigger(stream, channel, rhs_manual_trigger_source(config.trigger), false, true, false);
    controller.configureStimPulses(stream, channel, config.pulses,
                                  interphase ? BiphasicWithInterphaseDelay : Biphasic, true);
    controller.programStimReg(stream, channel, AbstractRHXController::EventAmpSettleOn, never);
    controller.programStimReg(stream, channel, AbstractRHXController::EventStartStim, 0);
    controller.programStimReg(stream, channel, AbstractRHXController::EventStimPhase2, config.phase_steps);
    controller.programStimReg(stream, channel, AbstractRHXController::EventStimPhase3,
                              interphase ? second_phase_start : never);
    controller.programStimReg(stream, channel, AbstractRHXController::EventEndStim, end_stim);
    controller.programStimReg(stream, channel, AbstractRHXController::EventRepeatStim,
                              config.pulses > 1 ? config.period_steps : never);
    controller.programStimReg(stream, channel, AbstractRHXController::EventAmpSettleOff, 0);
    controller.programStimReg(stream, channel, AbstractRHXController::EventChargeRecovOn, never);
    controller.programStimReg(stream, channel, AbstractRHXController::EventChargeRecovOff, 0);
    controller.programStimReg(stream, channel, AbstractRHXController::EventAmpSettleOnRepeat, never);
    controller.programStimReg(stream, channel, AbstractRHXController::EventAmpSettleOffRepeat, never);
    controller.programStimReg(stream, channel, AbstractRHXController::EventEnd, end_sequence);
}

template <typename Controller>
void enable_rhs_stim_sequence(Controller& controller, int stream, const RhsStimConfig& config)
{
    controller.setManualStimTrigger(config.trigger, false);
    controller.configureStimTrigger(stream, config.channel, rhs_manual_trigger_source(config.trigger), true, true, false);
}

template <typename Controller>
void disable_rhs_stim_stream(Controller& controller, int stream)
{
    for (int channel = 0; channel < 16; ++channel) {
        controller.configureStimTrigger(stream, channel, 0, false, true, false);
    }
}
