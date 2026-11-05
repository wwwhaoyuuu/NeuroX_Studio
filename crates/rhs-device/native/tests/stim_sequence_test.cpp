#include "rhs_stim_sequence.h"
#include "rhs_stream_selection.h"
#include "rhs_impedance_math.h"

#include <iostream>
#include <map>
#include <stdexcept>
#include <tuple>

struct SimulatedController {
    struct Trigger { int source; bool enabled; bool edge; bool low; };
    struct Pulse { int count; StimShape shape; bool negative_first; };
    std::map<std::pair<int, int>, Trigger> triggers;
    std::map<std::pair<int, int>, Pulse> pulses;
    std::map<std::tuple<int, int, int>, int> events;
    std::map<int, bool> manual_triggers;
    std::map<std::pair<int, int>, int> fired;

    void configureStimTrigger(int stream, int channel, int source, bool enabled, bool edge, bool low)
    {
        triggers[{stream, channel}] = {source, enabled, edge, low};
    }
    void configureStimPulses(int stream, int channel, int count, StimShape shape, bool negative_first)
    {
        pulses[{stream, channel}] = {count, shape, negative_first};
    }
    void programStimReg(int stream, int channel, AbstractRHXController::StimRegister reg, int value)
    {
        events[{stream, channel, reg}] = value;
    }
    void setManualStimTrigger(int trigger, bool enabled)
    {
        const bool rising = enabled && !manual_triggers[trigger];
        manual_triggers[trigger] = enabled;
        if (!rising) return;
        for (const auto& entry : triggers) {
            const auto& source = entry.second;
            if (source.enabled && source.edge && !source.low && source.source == 24 + trigger) {
                ++fired[entry.first];
            }
        }
    }
};

void require(bool condition, const char* message)
{
    if (!condition) throw std::runtime_error(message);
}

RhsScanResult make_scan(std::initializer_list<int> streams)
{
    RhsScanResult scan{};
    for (int stream : streams) {
        const int index = scan.chip_count++;
        scan.chip_type[index] = RHS2116Chip;
        scan.command_stream[index] = stream;
        scan.port_index[index] = stream / 2;
    }
    return scan;
}

void test_acquisition_selection()
{
    std::vector<int> selected;
    std::string error;
    const std::vector<int> expected{2, 3, 4, 5};
    auto four = make_scan({2, 3, 4, 5});
    auto eight = make_scan({0, 1, 2, 3, 4, 5, 6, 7});
    auto shuffled = make_scan({7, 5, 0, 3, 6, 2, 1, 4});
    for (const auto& scan : {four, eight, shuffled}) {
        require(select_rhs_acquisition_streams(scan, 64, selected, error) && selected == expected,
                "MEA 64 must select B/C regardless of extra ports or scan order");
    }
    require(select_rhs_acquisition_streams(eight, 16, selected, error) && selected == std::vector<int>({0}),
            "single-chip mode must retain the first detected physical stream");
    require(!select_rhs_acquisition_streams(make_scan({0, 1, 2, 4, 5, 6, 7}), 64, selected, error) && selected.empty(),
            "missing B3 must never be substituted with an A/D stream");
    auto invalid = eight;
    invalid.port_index[3] = 0;
    require(!select_rhs_acquisition_streams(invalid, 64, selected, error), "mismatched port/stream must fail");
    invalid = eight;
    invalid.command_stream[7] = 0;
    invalid.port_index[7] = 0;
    require(!select_rhs_acquisition_streams(invalid, 64, selected, error), "duplicate stream must fail");
    invalid = eight;
    invalid.chip_type[3] = RHD2216Chip;
    require(!select_rhs_acquisition_streams(invalid, 64, selected, error), "RHD must not substitute RHS2116");
    invalid = eight;
    invalid.chip_count = RHS_SCAN_MAX_RESULTS + 1;
    require(!select_rhs_acquisition_streams(invalid, 64, selected, error), "oversized scan must fail");
    require(!select_rhs_acquisition_streams(eight, 120, selected, error), "unsupported channel count must fail");
    require(!select_rhs_acquisition_streams(RhsScanResult{}, 16, selected, error), "empty scan must fail");

    require(select_rhs_acquisition_streams(eight, 64, selected, error), "eight-chip selection failed");
    SimulatedController controller;
    RhsStimConfig config{};
    for (int stream = 0; stream < 8; ++stream) disable_rhs_stim_stream(controller, stream);
    for (int stream : selected) config.stream_masks[stream] = 0xffff;
    enable_rhs_stim_routes(controller, selected, selected.front(), config);
    for (int stream = 0; stream < 8; ++stream) {
        for (int channel = 0; channel < 16; ++channel) {
            require(controller.triggers.at({stream, channel}).enabled == (stream >= 2 && stream <= 5),
                    "A/D stimulation must remain disabled in the B/C profile");
        }
    }
}

int main()
{
    try {
        const auto p = rhs_impedance::plan(1000.0);
        require(p.period == 30 && p.periods == 20 && p.blocks == 6, "Intan impedance timing plan");
        require(rhs_impedance::plan(1234).frequency == 1250, "actual impedance frequency must be quantized");
        for (double bad : {0.0, 9.0, 2001.0}) {
            bool rejected = false;
            try { rhs_impedance::plan(bad); } catch (const std::invalid_argument&) { rejected = true; }
            require(rejected, "invalid impedance frequency must fail");
        }
        std::vector<double> wave(p.blocks * 128);
        for (size_t i = 0; i < wave.size(); ++i)
            wave[i] = 321.0 + 123.0 * std::cos(2 * rhs_impedance::pi * i / p.period - 0.7);
        const auto amplitude = rhs_impedance::correlate(wave, p);
        require(std::abs(amplitude.magnitude - 123) < 1e-8 && std::abs(amplitude.phase + 0.7) < 1e-8,
            "correlation must recover magnitude and phase while rejecting DC");
        std::array<rhs_impedance::Polar, 3> voltages;
        const double omega = 2 * rhs_impedance::pi * p.frequency;
        const double ratio = omega * 12e-12 * 100000;
        const double parallel_r = 100000 / (1 + ratio * ratio);
        const double parallel_x = -ratio * parallel_r;
        const double caps[] = {0.1e-12, 1e-12, 10e-12};
        for (int i = 0; i < 3; ++i) voltages[i] = {
            std::hypot(parallel_r, parallel_x) * omega * 0.6125 * caps[i] * 1e6 / (1 + 18 * std::pow(p.frequency / 30000, 2)),
            std::atan2(parallel_x, parallel_r) - 2 * rhs_impedance::pi * 3 / p.period};
        const auto impedance = rhs_impedance::estimate(voltages, p, 7500);
        require(impedance.quality == 0 && std::abs(impedance.magnitude - 110000) < 1e-5 && std::abs(impedance.phase) < 1e-8,
            "RHS estimation must remove 12 pF and SPI delay, retaining Intan 1.1 correction");
        for (auto& value : voltages) value.magnitude = 6000;
        require(rhs_impedance::estimate(voltages, p, 7500).quality == 1, "saturated impedance must be flagged");
        for (auto& value : voltages) value.magnitude = 0;
        require(rhs_impedance::estimate(voltages, p, 7500).quality == 2, "low-signal impedance must be flagged");
        std::cout << "PASS: Intan impedance timing, correlation, range selection, parasitic/SPI correction and quality flags; synthetic inputs only\n";
        test_acquisition_selection();
        SimulatedController trigger_fixture;
        trigger_fixture.configureStimTrigger(2, 0, 0, true, true, false);
        trigger_fixture.setManualStimTrigger(0, true);
        require(trigger_fixture.fired.empty(), "manual trigger 0 must not activate digital input source 0");
        trigger_fixture.setManualStimTrigger(0, false);
        trigger_fixture.configureStimTrigger(2, 0, 24, true, true, false);
        trigger_fixture.setManualStimTrigger(0, true);
        require(trigger_fixture.fired.at({2, 0}) == 1, "manual F1 must activate FPGA source 24");
        require(rhs_manual_trigger_source(0) == 24 && rhs_manual_trigger_source(7) == 31,
                "manual F1/F8 source indices must match Intan");
        SimulatedController controller;
        RhsStimConfig config{};
        config.trigger = 3;
        config.pulses = 1;
        config.phase_steps = 6;
        config.interphase_steps = 3;
        config.period_steps = 3000;

        for (int stream : {2, 3, 4, 5}) {
            for (int channel = 0; channel < 16; ++channel) {
                config.channel = channel;
                program_rhs_stim_sequence(controller, stream, channel, config);
                const auto address = std::make_pair(stream, channel);
                require(!controller.triggers.at(address).enabled, "configuration must remain disabled");
                require(controller.pulses.at(address).shape == BiphasicWithInterphaseDelay,
                        "interphase delay requires Intan's delayed biphasic shape");
                require(controller.pulses.at(address).negative_first, "first phase polarity changed");
                require(controller.events.at({stream, channel, AbstractRHXController::EventStimPhase2}) == 6,
                        "first phase must end at six samples");
                require(controller.events.at({stream, channel, AbstractRHXController::EventStimPhase3}) == 9,
                        "second phase must start after the three-sample gap");
                require(controller.events.at({stream, channel, AbstractRHXController::EventEndStim}) == 15,
                        "second phase duration changed");
                require(controller.events.at({stream, channel, AbstractRHXController::EventRepeatStim}) == 65535,
                        "single pulse must not repeat");
                require(controller.events.at({stream, channel, AbstractRHXController::EventEnd}) == 3255,
                        "sequence must have a finite end");
                enable_rhs_stim_sequence(controller, stream, config);
                const auto trigger = controller.triggers.at(address);
                require(trigger.enabled && trigger.edge && !trigger.low && trigger.source == 27,
                        "explicit start must enable the selected rising-edge trigger");
                require(!controller.manual_triggers.at(3), "enable must first clear stale manual trigger");
                controller.setManualStimTrigger(3, true);
                require(controller.fired.at(address) == 1, "manual trigger must execute the selected physical channel");
                controller.setManualStimTrigger(3, false);
                disable_rhs_stim_stream(controller, stream);
                for (int output = 0; output < 16; ++output) {
                    require(!controller.triggers.at({stream, output}).enabled,
                            "stop must disable all channels on the selected chip");
                }
            }
        }
        require(controller.pulses.size() == 64, "four chips must retain 64 distinct physical addresses");
        require(controller.events.size() == 64 * 12, "each channel must program all twelve timing events");

        for (int stream : {2, 3, 4, 5}) config.stream_masks[stream] = 0xffff;
        enable_rhs_stim_routes(controller, std::initializer_list<int>{2, 3, 4, 5}, 2, config);
        for (int stream : {2, 3, 4, 5}) {
            for (int channel = 0; channel < 16; ++channel) {
                require(controller.triggers.at({stream, channel}).enabled, "64-channel group must enable each physical address");
            }
        }
        for (int stream : {2, 3, 4, 5}) config.stream_masks[stream] = 0;
        config.stream_masks[5] = 0x8000;
        enable_rhs_stim_routes(controller, std::initializer_list<int>{2, 3, 4, 5}, 2, config);
        for (int stream : {2, 3, 4, 5}) {
            for (int channel = 0; channel < 16; ++channel) {
                require(controller.triggers.at({stream, channel}).enabled == (stream == 5 && channel == 15), "selection must not enable unselected outputs");
            }
        }

        config.channel = 15;
        config.interphase_steps = 0;
        config.pulses = 2;
        program_rhs_stim_sequence(controller, 5, 15, config);
        require(controller.pulses.at({5, 15}).shape == Biphasic, "zero gap requires ordinary biphasic shape");
        require(controller.events.at({5, 15, AbstractRHXController::EventStimPhase3}) == 65535,
                "ordinary biphasic must not use phase three");
        require(controller.events.at({5, 15, AbstractRHXController::EventEndStim}) == 12,
                "zero-gap pulse must have twelve samples");
        require(controller.events.at({5, 15, AbstractRHXController::EventRepeatStim}) == 3000,
                "train must repeat at the configured period");
        require(controller.events.at({5, 15, AbstractRHXController::EventEnd}) == 6255,
                "train must retain its bounded end");
        std::cout << "PASS: four/eight-chip stream selection, A/D isolation, 64 physical addresses, Intan biphasic timing and stop; no hardware output\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << "FAIL: " << error.what() << '\n';
        return 1;
    }
}
