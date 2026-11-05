#include "rhs_scan_bridge.h"
#include "rhs_stim_sequence.h"
#include "rhs_stream_selection.h"
#include "rhs_impedance_math.h"

#include "rhxcontroller.h"
#include "rhxregisters.h"

#include <algorithm>
#include <cstdint>
#include <exception>
#include <memory>
#include <sstream>
#include <string>
#include <thread>
#include <vector>
#include <chrono>

struct RhsAcquisitionSession {
    std::unique_ptr<RHXController> controller;
    int target_stream;
    int target_port;
    int channel_count;
    std::vector<int> streams;
    int samples_per_block;
    int blocks_per_read;
    int bytes_per_block;
    std::string device_serial;
    bool running;
    bool stim_configured;
    bool stim_active;
    RhsStimConfig stim_config;
    std::vector<uint8_t> raw_buffer;
};

struct RhsStimSession {
    std::unique_ptr<RHXController> controller;
    int target_stream;
    int target_port;
    int active_trigger;
    bool running;
    bool requires_reopen;
};

namespace {

void set_message(RhsScanResult* out, const std::string& message)
{
    if (!out) return;
    std::fill(std::begin(out->message), std::end(out->message), '\0');
    const size_t n = std::min(message.size(), sizeof(out->message) - 1);
    std::copy_n(message.data(), n, out->message);
}

void reset_result(RhsScanResult* out)
{
    if (!out) return;
    out->return_code = -100;
    out->chip_count = 0;
    out->pipe_read_error = 0;
    std::fill(std::begin(out->chip_type), std::end(out->chip_type), -1);
    std::fill(std::begin(out->port_index), std::end(out->port_index), -1);
    std::fill(std::begin(out->command_stream), std::end(out->command_stream), -1);
    std::fill(std::begin(out->channels_on_port), std::end(out->channels_on_port), 0);
    set_message(out, "");
}

const char* chip_type_name(int chip_type)
{
    switch (chip_type) {
    case RHS2116Chip: return "RHS2116";
    case RHD2132Chip: return "RHD2132";
    case RHD2216Chip: return "RHD2216";
    case RHD2164Chip: return "RHD2164";
    case RHD2164MISOBChip: return "RHD2164_MISO_B";
    case NoChip: return "NoChip";
    default: return "Unknown";
    }
}

void reset_acquisition_info(RhsAcquisitionInfo* out)
{
    if (!out) return;
    out->return_code = -100;
    out->sample_rate_hz = 0;
    out->channel_count = 0;
    out->samples_per_block = 0;
    out->blocks_per_read = 0;
    out->target_stream = -1;
    out->target_port = -1;
    out->bytes_per_block = 0;
    out->pipe_read_error = 0;
    std::fill(std::begin(out->device_serial), std::end(out->device_serial), '\0');
    std::fill(std::begin(out->message), std::end(out->message), '\0');
}

void set_acquisition_message(RhsAcquisitionInfo* out, const std::string& message)
{
    if (!out) return;
    std::fill(std::begin(out->message), std::end(out->message), '\0');
    const size_t n = std::min(message.size(), sizeof(out->message) - 1);
    std::copy_n(message.data(), n, out->message);
}

void reset_read_result(RhsAcquisitionReadResult* out)
{
    if (!out) return;
    out->return_code = -100;
    out->block_count = 0;
    out->samples_per_channel = 0;
    out->invalid_header_count = 0;
    out->first_timestamp = 0;
    out->last_timestamp = 0;
    out->pipe_read_error = 0;
    out->bytes_read = 0;
    std::fill(std::begin(out->message), std::end(out->message), '\0');
}

void set_read_message(RhsAcquisitionReadResult* out, const std::string& message)
{
    if (!out) return;
    std::fill(std::begin(out->message), std::end(out->message), '\0');
    const size_t n = std::min(message.size(), sizeof(out->message) - 1);
    std::copy_n(message.data(), n, out->message);
}

void reset_acquisition_stim_result(RhsAcquisitionStimResult* out)
{
    if (!out) return;
    out->return_code = -100;
    out->active = 0;
    out->pipe_read_error = 0;
    std::fill(std::begin(out->message), std::end(out->message), '\0');
}

void fill_acquisition_stim_result(
    const RhsAcquisitionSession* session,
    RhsAcquisitionStimResult* out,
    int return_code,
    const std::string& message
)
{
    if (!out) return;
    reset_acquisition_stim_result(out);
    out->return_code = return_code;
    if (session && session->controller) {
        out->active = session->stim_active ? 1 : 0;
        out->pipe_read_error = session->controller->pipeReadError();
    }
    const size_t n = std::min(message.size(), sizeof(out->message) - 1);
    std::copy_n(message.data(), n, out->message);
}

void fill_acquisition_info(
    const RhsAcquisitionSession* session,
    RhsAcquisitionInfo* out,
    int return_code,
    const std::string& message
)
{
    if (!out) return;
    reset_acquisition_info(out);
    out->return_code = return_code;
    if (session) {
        out->sample_rate_hz = static_cast<int>(session->controller->getSampleRate());
        out->channel_count = session->channel_count;
        out->samples_per_block = session->samples_per_block;
        out->blocks_per_read = session->blocks_per_read;
        out->target_stream = session->target_stream;
        out->target_port = session->target_port;
        out->bytes_per_block = session->bytes_per_block;
        out->pipe_read_error = session->controller->pipeReadError();
        const size_t serial_length = std::min(
            session->device_serial.size(),
            sizeof(out->device_serial) - 1
        );
        std::copy_n(session->device_serial.data(), serial_length, out->device_serial);
    }
    set_acquisition_message(out, message);
}

} // namespace

int rhs_scan_connected_chips(const char* bitfile_path, const char* serial, RhsScanResult* out)
{
    reset_result(out);
    if (!out) return -101;
    if (!bitfile_path || bitfile_path[0] == '\0') {
        set_message(out, "missing bitfile path");
        return -102;
    }

    try {
        RHXController controller(ControllerStimRecord, SampleRate30000Hz, true);

        std::vector<std::string> serials = controller.listAvailableDeviceSerials();
        if (serials.empty()) {
            out->return_code = -1;
            set_message(out, "no FrontPanel device detected");
            return out->return_code;
        }

        std::string chosen_serial = serial && serial[0] != '\0' ? std::string(serial) : serials[0];
        int open_code = controller.open(chosen_serial);
        if (open_code != 1) {
            out->return_code = -2;
            set_message(out, "failed to open FrontPanel device");
            return out->return_code;
        }

        if (!controller.uploadFPGABitfile(bitfile_path)) {
            out->return_code = -3;
            set_message(out, "failed to configure FPGA bitfile");
            return out->return_code;
        }

        controller.initialize();
        controller.setStimCmdMode(false);
        controller.resetSequencers();

        RHXRegisters chip_registers(controller.getType(), controller.getSampleRate());
        std::vector<unsigned int> command_list;
        int command_length = chip_registers.createCommandListRHSRegisterRead(command_list);
        if (command_length <= 0) {
            out->return_code = -4;
            set_message(out, "failed to create RHS register-read command list");
            return out->return_code;
        }

        controller.uploadCommandList(command_list, RHXController::AuxCmd1);
        controller.selectAuxCommandLength(RHXController::AuxCmd1, 0, command_length - 1);

        std::vector<ChipType> chip_type;
        std::vector<int> port_index;
        std::vector<int> command_stream;
        std::vector<int> channels_on_port;

        int scan_code = controller.findConnectedChips(
            chip_type,
            port_index,
            command_stream,
            channels_on_port,
            false
        );

        out->return_code = scan_code;
        out->pipe_read_error = controller.pipeReadError();

        int found = 0;
        for (size_t i = 0; i < chip_type.size() && found < RHS_SCAN_MAX_RESULTS; ++i) {
            if (chip_type[i] != NoChip) {
                out->chip_type[found] = static_cast<int>(chip_type[i]);
                out->port_index[found] = i < port_index.size() ? port_index[i] : -1;
                out->command_stream[found] = i < command_stream.size() ? command_stream[i] : -1;
                ++found;
            }
        }
        out->chip_count = found;

        for (size_t i = 0; i < channels_on_port.size() && i < 4; ++i) {
            out->channels_on_port[i] = channels_on_port[i];
        }

        std::ostringstream message;
        if (found == 0) {
            message << "scan completed; no RHS2116/RHD chip detected";
        } else {
            message << "scan completed; found ";
            for (int i = 0; i < found; ++i) {
                if (i > 0) message << ", ";
                message << chip_type_name(out->chip_type[i])
                        << " on port " << char('A' + out->port_index[i])
                        << " command_stream=" << out->command_stream[i];
            }
        }
        set_message(out, message.str());
        return out->return_code;
    } catch (const std::exception& e) {
        out->return_code = -98;
        set_message(out, std::string("exception: ") + e.what());
        return out->return_code;
    } catch (...) {
        out->return_code = -99;
        set_message(out, "unknown exception");
        return out->return_code;
    }
}

namespace {

bool prepare_controller(
    RHXController& controller,
    const char* bitfile_path,
    const char* serial,
    RhsScanResult* out,
    int& target_stream,
    int& target_port,
    int expected_port = -1,
    int expected_stream = -1
)
{
    std::vector<std::string> serials = controller.listAvailableDeviceSerials();
    if (serials.empty()) {
        out->return_code = -1;
        set_message(out, "no FrontPanel device detected");
        return false;
    }

    std::string chosen_serial = serial && serial[0] != '\0' ? std::string(serial) : serials[0];
    int open_code = controller.open(chosen_serial);
    if (open_code != 1) {
        out->return_code = -2;
        set_message(out, "failed to open FrontPanel device");
        return false;
    }

    if (!controller.uploadFPGABitfile(bitfile_path)) {
        out->return_code = -3;
        set_message(out, "failed to configure FPGA bitfile");
        return false;
    }

    controller.initialize();
    controller.setStimCmdMode(false);
    controller.resetSequencers();

    RHXRegisters chip_registers(controller.getType(), controller.getSampleRate());
    std::vector<unsigned int> command_list;
    int command_length = chip_registers.createCommandListRHSRegisterRead(command_list);
    controller.uploadCommandList(command_list, RHXController::AuxCmd1);
    controller.selectAuxCommandLength(RHXController::AuxCmd1, 0, command_length - 1);

    std::vector<ChipType> chip_type;
    std::vector<int> port_index;
    std::vector<int> command_stream;
    std::vector<int> channels_on_port;
    int scan_code = controller.findConnectedChips(chip_type, port_index, command_stream, channels_on_port, false);

    out->return_code = scan_code;
    out->pipe_read_error = controller.pipeReadError();

    if (scan_code < 0 || out->pipe_read_error != 0) {
        out->return_code = -104;
        set_message(out, "chip scan failed or USB pipe reported an error");
        return false;
    }
    for (size_t i = 0; i < chip_type.size() && i < RHS_SCAN_MAX_RESULTS; ++i) {
        out->chip_type[i] = chip_type[i];
        out->port_index[i] = port_index[i];
        out->command_stream[i] = command_stream[i];
        ++out->chip_count;
    }

    target_stream = -1;
    target_port = -1;
    for (size_t i = 0; i < chip_type.size(); ++i) {
        if (chip_type[i] == RHS2116Chip &&
            (expected_port < 0 || port_index[i] == expected_port) &&
            (expected_stream < 0 || command_stream[i] == expected_stream)) {
            target_stream = command_stream[i];
            target_port = port_index[i];
            break;
        }
    }

    if (target_stream < 0) {
        out->return_code = -5;
        std::ostringstream message;
        message << "scan completed, but no RHS2116 matched port=" << expected_port
                << " stream=" << expected_stream;
        set_message(out, message.str());
        return false;
    }

    return true;
}

bool run_aux_command_list(RHXController& controller, const std::vector<unsigned int>& command_list)
{
    const int command_length = static_cast<int>(command_list.size());
    if (command_length <= 0) return false;

    controller.uploadCommandList(command_list, RHXController::AuxCmd1);
    controller.selectAuxCommandLength(RHXController::AuxCmd1, 0, command_length - 1);
    controller.setMaxTimeStep(command_length);
    controller.setContinuousRunMode(false);
    controller.run();
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (controller.isRunning()) {
        if (std::chrono::steady_clock::now() >= deadline) {
            controller.setStimCmdMode(false);
            controller.resetSequencers();
            controller.setContinuousRunMode(false);
            controller.setMaxTimeStep(0);
            return false;
        }
        std::this_thread::sleep_for(std::chrono::microseconds(100));
    }

    RHXDataBlock data_block(controller.getType(), controller.getNumEnabledDataStreams());
    return controller.readDataBlock(&data_block) && controller.pipeReadError() == 0;
}

bool validate_stim_config(const RhsStimConfig* config, RhsScanResult* out)
{
    if (!config) {
        out->return_code = -102;
        set_message(out, "missing stimulation config");
        return false;
    }
    if (config->channel < 0 || config->channel > 15 ||
        config->channel_mask < 0 || config->channel_mask > 0xffff ||
        config->trigger < 0 || config->trigger > 7 ||
        config->pulses < 1 || config->pulses > 20 ||
        config->magnitude_steps < 1 || config->magnitude_steps > 255 ||
        config->stim_step_size < 1 || config->stim_step_size > 10 ||
        config->phase_steps < 1 || config->phase_steps > 1000 ||
        config->interphase_steps < 0 || config->interphase_steps > 1000 ||
        config->period_steps < 10 || config->period_steps > 30000) {
        out->return_code = -10;
        set_message(out, "stimulation config outside allowed bridge limits");
        return false;
    }
    if (config->channel_mask != 0 &&
        (config->channel_mask & (1 << config->channel)) == 0) {
        out->return_code = -10;
        set_message(out, "stimulation channel mask must include primary channel");
        return false;
    }

    const int end_stim = 2 * config->phase_steps + config->interphase_steps;
    const int total_steps = config->period_steps * config->pulses + 256;
    if (end_stim >= config->period_steps || total_steps > 65534) {
        out->return_code = -11;
        set_message(out, "timing invalid: pulse must fit inside period and total sequence must fit FPGA limit");
        return false;
    }
    return true;
}

int effective_stim_channel_mask(const RhsStimConfig& config)
{
    return config.channel_mask != 0 ? config.channel_mask : (1 << config.channel);
}

int acquisition_stim_mask(const RhsAcquisitionSession& session, int stream)
{
    return routed_rhs_stim_mask(session.stim_config, stream, session.target_stream);
}

void fill_stim_result(
    const RhsStimSession* session,
    RhsScanResult* out,
    int return_code,
    const std::string& message
)
{
    reset_result(out);
    if (!out) return;
    out->return_code = return_code;
    if (session && session->controller) {
        out->pipe_read_error = session->controller->pipeReadError();
        if (session->target_stream >= 0 && session->target_port >= 0) {
            out->chip_count = 1;
            out->chip_type[0] = RHS2116Chip;
            out->port_index[0] = session->target_port;
            out->command_stream[0] = session->target_stream;
            if (session->target_port < 4) {
                out->channels_on_port[session->target_port] = 16;
            }
        }
    }
    set_message(out, message);
}

bool stop_stim_output(RhsStimSession* session, bool& board_was_reset)
{
    board_was_reset = false;
    if (!session || !session->controller) return false;

    for (int trigger = 0; trigger < 8; ++trigger) {
        session->controller->setManualStimTrigger(trigger, false);
    }
    session->controller->setStimCmdMode(false);
    disable_rhs_stim_stream(*session->controller, session->target_stream);
    session->controller->resetSequencers();
    session->controller->setContinuousRunMode(false);
    session->controller->setMaxTimeStep(0);

    for (int attempt = 0; attempt < 10 && session->controller->isRunning(); ++attempt) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    if (session->controller->isRunning()) {
        session->controller->resetBoard();
        board_was_reset = true;
    }
    session->running = false;
    session->active_trigger = -1;
    session->requires_reopen = session->requires_reopen || board_was_reset;
    return true;
}

} // namespace

int rhs_stim_open(
    const char* bitfile_path,
    const char* serial,
    RhsStimSession** out_session,
    RhsScanResult* out
)
{
    return rhs_stim_open_target(bitfile_path, serial, -1, -1, out_session, out);
}

int rhs_stim_open_target(
    const char* bitfile_path,
    const char* serial,
    int expected_port,
    int expected_stream,
    RhsStimSession** out_session,
    RhsScanResult* out
)
{
    reset_result(out);
    if (out_session) *out_session = nullptr;
    if (!out_session || !out) return -101;
    if (!bitfile_path || bitfile_path[0] == '\0') {
        out->return_code = -103;
        set_message(out, "missing bitfile path");
        return out->return_code;
    }
    if ((expected_port >= 0 || expected_stream >= 0) &&
        (expected_port < 0 || expected_port > 3 ||
         expected_stream < 0 || expected_stream > 7)) {
        out->return_code = -104;
        set_message(out, "targeted stimulation requires a valid port (0..3) and stream (0..7)");
        return out->return_code;
    }

    try {
        std::unique_ptr<RhsStimSession> session(new RhsStimSession());
        session->controller.reset(new RHXController(ControllerStimRecord, SampleRate30000Hz, true));
        session->target_stream = -1;
        session->target_port = -1;
        session->active_trigger = -1;
        session->running = false;
        session->requires_reopen = false;

        if (!prepare_controller(
                *session->controller,
                bitfile_path,
                serial,
                out,
                session->target_stream,
                session->target_port,
                expected_port,
                expected_stream)) {
            return out->return_code;
        }

        for (int stream = 0; stream < session->controller->maxNumDataStreams(); ++stream) {
            session->controller->enableDataStream(stream, false);
        }
        session->controller->enableDataStream(session->target_stream, true);
        for (int trigger = 0; trigger < 8; ++trigger) {
            session->controller->setManualStimTrigger(trigger, false);
        }
        session->controller->setStimCmdMode(false);
        session->controller->resetSequencers();
        session->controller->setContinuousRunMode(false);
        session->controller->setMaxTimeStep(0);

        std::ostringstream message;
        message << "stimulation session ready with output disabled on RHS2116 port "
                << char('A' + session->target_port)
                << ", stream=" << session->target_stream;
        fill_stim_result(session.get(), out, 1, message.str());
        *out_session = session.release();
        return 1;
    } catch (const std::exception& e) {
        out->return_code = -98;
        set_message(out, std::string("exception: ") + e.what());
        return out->return_code;
    } catch (...) {
        out->return_code = -99;
        set_message(out, "unknown exception");
        return out->return_code;
    }
}

int rhs_stim_start(
    RhsStimSession* session,
    const RhsStimConfig* config,
    RhsScanResult* out
)
{
    reset_result(out);
    if (!session || !out) return -101;
    if (!validate_stim_config(config, out)) return out->return_code;
    if (effective_stim_channel_mask(*config) != (1 << config->channel)) {
        fill_stim_result(session, out, -10, "direct stimulation session supports one channel; use acquisition session for channel masks");
        return out->return_code;
    }
    if (session->running) {
        fill_stim_result(session, out, -12, "a stimulation command is already active");
        return out->return_code;
    }
    if (session->requires_reopen) {
        fill_stim_result(session, out, -13, "session requires close/reopen after emergency board reset");
        return out->return_code;
    }

    try {
        StimStepSize step = static_cast<StimStepSize>(config->stim_step_size);
        session->controller->setStimCmdMode(false);
        session->controller->enableAuxCommandsOnOneStream(session->target_stream);
        RHXRegisters regs(session->controller->getType(), session->controller->getSampleRate(), step);
        std::vector<unsigned int> command_list;

        int len = regs.createCommandListRHSRegisterConfig(command_list, true);
        if (len <= 0 || !run_aux_command_list(*session->controller, command_list)) {
            fill_stim_result(session, out, -20, "failed to program RHS baseline register config");
            return out->return_code;
        }

        len = regs.createCommandListSetStimMagnitudes(
            command_list,
            config->channel,
            config->magnitude_steps,
            0,
            config->magnitude_steps,
            0
        );
        if (len <= 0 || !run_aux_command_list(*session->controller, command_list)) {
            fill_stim_result(session, out, -21, "failed to program RHS stimulation magnitudes");
            return out->return_code;
        }

        const int total_steps = config->period_steps * config->pulses + 256;
        program_rhs_stim_sequence(*session->controller, session->target_stream, config->channel, *config);

        session->active_trigger = config->trigger;
        session->running = true;
        session->controller->setMaxTimeStep(total_steps);
        session->controller->setContinuousRunMode(false);
        enable_rhs_stim_sequence(*session->controller, session->target_stream, *config);
        session->controller->setStimCmdMode(true);
        session->controller->run();
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
        session->controller->setManualStimTrigger(config->trigger, true);
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
        session->controller->setManualStimTrigger(config->trigger, false);

        std::ostringstream message;
        message << "bounded biphasic stimulation started on RHS2116 port "
                << char('A' + session->target_port)
                << ", stream=" << session->target_stream
                << ", channel=" << config->channel
                << ", pulses=" << config->pulses;
        fill_stim_result(session, out, 2, message.str());
        return 2;
    } catch (const std::exception& e) {
        bool board_was_reset = false;
        stop_stim_output(session, board_was_reset);
        fill_stim_result(session, out, -98, std::string("exception: ") + e.what());
        return out->return_code;
    } catch (...) {
        bool board_was_reset = false;
        stop_stim_output(session, board_was_reset);
        fill_stim_result(session, out, -99, "unknown exception");
        return out->return_code;
    }
}

int rhs_stim_poll(RhsStimSession* session, RhsScanResult* out)
{
    reset_result(out);
    if (!session || !out) return -101;
    if (!session->running) {
        fill_stim_result(session, out, 1, "no stimulation command is active");
        return 1;
    }

    try {
        if (session->controller->isRunning()) {
            fill_stim_result(session, out, 2, "stimulation command is running");
            return 2;
        }

        session->controller->setManualStimTrigger(session->active_trigger, false);
        session->controller->setStimCmdMode(false);
        disable_rhs_stim_stream(*session->controller, session->target_stream);
        session->controller->resetSequencers();
        session->running = false;
        session->active_trigger = -1;
        fill_stim_result(session, out, 1, "bounded biphasic stimulation completed");
        return 1;
    } catch (const std::exception& e) {
        bool board_was_reset = false;
        stop_stim_output(session, board_was_reset);
        fill_stim_result(session, out, -98, std::string("exception: ") + e.what());
        return out->return_code;
    } catch (...) {
        bool board_was_reset = false;
        stop_stim_output(session, board_was_reset);
        fill_stim_result(session, out, -99, "unknown exception");
        return out->return_code;
    }
}

int rhs_stim_cancel(RhsStimSession* session, RhsScanResult* out)
{
    reset_result(out);
    if (!session || !out) return -101;
    try {
        const bool was_running = session->running;
        bool board_was_reset = false;
        stop_stim_output(session, board_was_reset);

        std::ostringstream message;
        if (was_running) {
            message << "active stimulation command cancelled";
        } else {
            message << "idle stimulation session forced to output-disabled state";
        }
        if (board_was_reset) {
            message << "; board reset was required and the session must be reopened";
        }
        fill_stim_result(session, out, 1, message.str());
        return 1;
    } catch (const std::exception& e) {
        fill_stim_result(session, out, -98, std::string("exception: ") + e.what());
        return out->return_code;
    } catch (...) {
        fill_stim_result(session, out, -99, "unknown exception");
        return out->return_code;
    }
}

void rhs_stim_close(RhsStimSession* session)
{
    if (!session) return;
    try {
        bool board_was_reset = false;
        stop_stim_output(session, board_was_reset);
    } catch (...) {
    }
    delete session;
}

int rhs_run_bounded_stim(
    const char* bitfile_path,
    const char* serial,
    const RhsStimConfig* config,
    RhsScanResult* out
)
{
    reset_result(out);
    if (!out) return -101;
    if (!config) {
        set_message(out, "missing stimulation config");
        return -102;
    }
    if (!bitfile_path || bitfile_path[0] == '\0') {
        set_message(out, "missing bitfile path");
        return -103;
    }

    if (config->channel < 0 || config->channel > 15 ||
        config->channel_mask < 0 || config->channel_mask > 0xffff ||
        config->trigger < 0 || config->trigger > 7 ||
        config->pulses < 1 || config->pulses > 20 ||
        config->magnitude_steps < 1 || config->magnitude_steps > 255 ||
        config->phase_steps < 1 || config->phase_steps > 1000 ||
        config->interphase_steps < 0 || config->interphase_steps > 1000 ||
        config->period_steps < 10 || config->period_steps > 30000) {
        out->return_code = -10;
        set_message(out, "stimulation config outside allowed PoC limits");
        return out->return_code;
    }
    if (effective_stim_channel_mask(*config) != (1 << config->channel)) {
        out->return_code = -10;
        set_message(out, "bounded stimulation supports one channel only");
        return out->return_code;
    }

    const int phase2_start = config->phase_steps + config->interphase_steps;
    const int end_stim = phase2_start + config->phase_steps;
    const int total_steps = config->period_steps * config->pulses + 256;

    if (end_stim >= config->period_steps || total_steps > 65534) {
        out->return_code = -11;
        set_message(out, "timing invalid: pulse must fit inside period and total sequence must fit FPGA limit");
        return out->return_code;
    }

    try {
        RHXController controller(ControllerStimRecord, SampleRate30000Hz, true);
        int target_stream = -1;
        int target_port = -1;
        if (!prepare_controller(controller, bitfile_path, serial, out, target_stream, target_port)) {
            return out->return_code;
        }

        for (int stream = 0; stream < controller.maxNumDataStreams(); ++stream) {
            controller.enableDataStream(stream, false);
        }
        controller.enableDataStream(target_stream, true);
        controller.enableAuxCommandsOnOneStream(target_stream);

        StimStepSize step = static_cast<StimStepSize>(config->stim_step_size);
        RHXRegisters regs(controller.getType(), controller.getSampleRate(), step);
        std::vector<unsigned int> command_list;

        int len = regs.createCommandListRHSRegisterConfig(command_list, true);
        if (len <= 0 || !run_aux_command_list(controller, command_list)) {
            out->return_code = -20;
            set_message(out, "failed to program RHS baseline register config");
            return out->return_code;
        }

        len = regs.createCommandListSetStimMagnitudes(
            command_list,
            config->channel,
            config->magnitude_steps,
            0,
            config->magnitude_steps,
            0
        );
        if (len <= 0 || !run_aux_command_list(controller, command_list)) {
            out->return_code = -21;
            set_message(out, "failed to program RHS stimulation magnitudes");
            return out->return_code;
        }

        program_rhs_stim_sequence(controller, target_stream, config->channel, *config);

        controller.setMaxTimeStep(total_steps);
        controller.setContinuousRunMode(false);
        enable_rhs_stim_sequence(controller, target_stream, *config);
        controller.setStimCmdMode(true);
        controller.run();
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
        controller.setManualStimTrigger(config->trigger, true);
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
        controller.setManualStimTrigger(config->trigger, false);

        while (controller.isRunning()) {
            std::this_thread::sleep_for(std::chrono::microseconds(100));
        }

        controller.setStimCmdMode(false);
        disable_rhs_stim_stream(controller, target_stream);
        controller.resetSequencers();

        out->chip_count = 1;
        out->chip_type[0] = RHS2116Chip;
        out->port_index[0] = target_port;
        out->command_stream[0] = target_stream;
        out->channels_on_port[target_port] = 16;
        out->return_code = 1;

        std::ostringstream message;
        message << "bounded biphasic stimulation completed on RHS2116 port "
                << char('A' + target_port)
                << ", stream=" << target_stream
                << ", channel=" << config->channel
                << ", pulses=" << config->pulses
                << ", magnitude_steps=" << config->magnitude_steps;
        set_message(out, message.str());
        return out->return_code;
    } catch (const std::exception& e) {
        out->return_code = -98;
        set_message(out, std::string("exception: ") + e.what());
        return out->return_code;
    } catch (...) {
        out->return_code = -99;
        set_message(out, "unknown exception");
        return out->return_code;
    }
}

int rhs_acquisition_open(
    const char* bitfile_path,
    const char* serial,
    int blocks_per_read,
    RhsAcquisitionSession** out_session,
    RhsAcquisitionInfo* out_info
)
{
    return rhs_acquisition_open_channels(bitfile_path, serial, blocks_per_read, 16, out_session, out_info);
}

int rhs_acquisition_streams(RhsAcquisitionSession* session, int* streams, size_t capacity)
{
    if (!session || !streams || capacity < session->streams.size()) return -111;
    std::copy(session->streams.begin(), session->streams.end(), streams);
    return static_cast<int>(session->streams.size());
}

int rhs_acquisition_open_channels(
    const char* bitfile_path, const char* serial, int blocks_per_read,
    int channel_count, RhsAcquisitionSession** out_session, RhsAcquisitionInfo* out_info
)
{
    reset_acquisition_info(out_info);
    if (out_session) *out_session = nullptr;
    if (!out_session || !out_info) return -101;
    if (!bitfile_path || bitfile_path[0] == '\0') {
        out_info->return_code = -102;
        set_acquisition_message(out_info, "missing bitfile path");
        return out_info->return_code;
    }
    if (blocks_per_read < 1 || blocks_per_read > 32) {
        out_info->return_code = -103;
        set_acquisition_message(out_info, "blocks_per_read must be 1..32");
        return out_info->return_code;
    }
    if (channel_count != 16 && channel_count != 64) {
        out_info->return_code = -103;
        set_acquisition_message(out_info, "acquisition channel count must be 16 or 64");
        return -103;
    }

    try {
        std::unique_ptr<RhsAcquisitionSession> session(new RhsAcquisitionSession());
        session->controller.reset(new RHXController(ControllerStimRecord, SampleRate30000Hz, true));
        session->target_stream = -1;
        session->target_port = -1;
        session->channel_count = channel_count;
        session->samples_per_block = RHXDataBlock::samplesPerDataBlock(ControllerStimRecord);
        session->blocks_per_read = blocks_per_read;
        session->bytes_per_block = static_cast<int>(
            BytesPerWord * RHXDataBlock::dataBlockSizeInWords(ControllerStimRecord, channel_count / 16)
        );
        session->running = false;
        session->stim_configured = false;
        session->stim_active = false;

        std::vector<std::string> available_serials = session->controller->listAvailableDeviceSerials();
        if (available_serials.empty()) {
            out_info->return_code = -1;
            set_acquisition_message(out_info, "no FrontPanel device detected");
            return out_info->return_code;
        }
        session->device_serial = serial && serial[0] != '\0' ?
            std::string(serial) : available_serials[0];

        RhsScanResult scan_result;
        reset_result(&scan_result);
        if (!prepare_controller(
                *session->controller,
                bitfile_path,
                session->device_serial.c_str(),
                &scan_result,
                session->target_stream,
                session->target_port)) {
            out_info->return_code = scan_result.return_code;
            out_info->pipe_read_error = scan_result.pipe_read_error;
            set_acquisition_message(out_info, scan_result.message);
            return out_info->return_code;
        }

        std::string selection_error;
        if (!select_rhs_acquisition_streams(scan_result, channel_count, session->streams, selection_error)) {
            out_info->return_code = -104;
            set_acquisition_message(out_info, selection_error);
            return -104;
        }
        session->target_stream = session->streams.front();
        session->target_port = session->target_stream / 2;
        for (int stream = 0; stream < session->controller->maxNumDataStreams(); ++stream) {
            session->controller->enableDataStream(stream, false);
            disable_rhs_stim_stream(*session->controller, stream);
        }
        for (int stream : session->streams) session->controller->enableDataStream(stream, true);

        if (session->controller->getNumEnabledDataStreams() != channel_count / 16) {
            out_info->return_code = -104;
            set_acquisition_message(out_info, "enabled RHS2116 stream count does not match requested channels");
            return out_info->return_code;
        }

        session->controller->enableAuxCommandsOnAllStreams();
        RHXRegisters registers(session->controller->getType(), session->controller->getSampleRate());
        std::vector<unsigned int> command_list;
        int command_length = registers.createCommandListRHSRegisterConfig(command_list, true);
        if (command_length <= 0) {
            out_info->return_code = -105;
            set_acquisition_message(out_info, "failed to create RHS acquisition register config");
            return out_info->return_code;
        }
        session->controller->uploadCommandList(command_list, RHXController::AuxCmd1);
        session->controller->selectAuxCommandLength(RHXController::AuxCmd1, 0, command_length - 1);

        registers.createCommandListDummy(
            command_list,
            8192,
            registers.createRHXCommand(RHXRegisters::RHXCommandRegRead, 255)
        );
        session->controller->uploadCommandList(command_list, RHXController::AuxCmd2);
        registers.createCommandListDummy(
            command_list,
            8192,
            registers.createRHXCommand(RHXRegisters::RHXCommandRegRead, 254)
        );
        session->controller->uploadCommandList(command_list, RHXController::AuxCmd3);
        registers.createCommandListDummy(
            command_list,
            8192,
            registers.createRHXCommand(RHXRegisters::RHXCommandRegRead, 253)
        );
        session->controller->uploadCommandList(command_list, RHXController::AuxCmd4);

        command_length = registers.createCommandListRHSRegisterConfig(command_list, true);
        if (!run_aux_command_list(*session->controller, command_list)) {
            out_info->return_code = -106;
            set_acquisition_message(out_info, "failed to apply RHS acquisition register config");
            return out_info->return_code;
        }

        session->controller->enableAuxCommandsOnAllStreams();
        session->controller->enableDcAmpConvert(true);
        session->controller->setStimCmdMode(false);
        session->controller->setContinuousRunMode(false);
        session->controller->setMaxTimeStep(0);
        session->raw_buffer.resize(
            static_cast<size_t>(session->bytes_per_block) * static_cast<size_t>(blocks_per_read)
        );

        std::ostringstream message;
        message << "acquisition session ready for RHS2116 port "
                << char('A' + session->target_port)
                << ", command_stream=" << session->target_stream
                << ", blocks_per_read=" << session->blocks_per_read;
        fill_acquisition_info(session.get(), out_info, 1, message.str());
        *out_session = session.release();
        return 1;
    } catch (const std::exception& e) {
        out_info->return_code = -198;
        set_acquisition_message(out_info, std::string("exception: ") + e.what());
        return out_info->return_code;
    } catch (...) {
        out_info->return_code = -199;
        set_acquisition_message(out_info, "unknown exception");
        return out_info->return_code;
    }
}

int rhs_acquisition_configure_stim(
    RhsAcquisitionSession* session,
    const RhsStimConfig* config,
    RhsAcquisitionStimResult* out_result
)
{
    reset_acquisition_stim_result(out_result);
    if (!session || !out_result) return -101;
    if (session->running) {
        fill_acquisition_stim_result(
            session,
            out_result,
            -120,
            "stimulation must be configured before continuous acquisition starts"
        );
        return out_result->return_code;
    }

    RhsScanResult validation;
    reset_result(&validation);
    if (!validate_stim_config(config, &validation)) {
        fill_acquisition_stim_result(
            session,
            out_result,
            validation.return_code,
            validation.message
        );
        return out_result->return_code;
    }

    session->stim_configured = false;
    session->stim_active = false;

    bool routed = false;
    for (int stream = 0; stream < RHS_SCAN_MAX_RESULTS; ++stream) {
        const int mask = config->stream_masks[stream];
        if (mask < 0 || mask > 0xffff || (mask != 0 &&
            std::find(session->streams.begin(), session->streams.end(), stream) == session->streams.end())) {
            fill_acquisition_stim_result(session, out_result, -103, "stimulation mask addresses an absent stream");
            return -103;
        }
        routed = routed || mask != 0;
    }
    if (session->streams.size() > 1 && !routed) {
        fill_acquisition_stim_result(session, out_result, -103, "multi-stream stimulation requires explicit stream masks");
        return -103;
    }
    session->stim_config = *config;

    try {
        session->controller->setStimCmdMode(false);
        for (int trigger = 0; trigger < 8; ++trigger) {
            session->controller->setManualStimTrigger(trigger, false);
        }
        for (int stream : session->streams) {
        disable_rhs_stim_stream(*session->controller, stream);
        const int channel_mask = acquisition_stim_mask(*session, stream);
        if (channel_mask == 0) continue;
        session->controller->enableAuxCommandsOnOneStream(stream);
        StimStepSize step = static_cast<StimStepSize>(config->stim_step_size);
        RHXRegisters registers(
            session->controller->getType(),
            session->controller->getSampleRate(),
            step
        );
        std::vector<unsigned int> command_list;

        int command_length = registers.createCommandListRHSRegisterConfig(command_list, true);
        if (command_length <= 0 || !run_aux_command_list(*session->controller, command_list)) {
            fill_acquisition_stim_result(
                session,
                out_result,
                -121,
                "failed to program RHS stimulation baseline before acquisition"
            );
            return out_result->return_code;
        }

        for (int channel = 0; channel < 16; ++channel) {
            if ((channel_mask & (1 << channel)) == 0) continue;
            command_length = registers.createCommandListSetStimMagnitudes(
                command_list,
                channel,
                config->magnitude_steps,
                0,
                config->magnitude_steps,
                0
            );
            if (command_length <= 0 || !run_aux_command_list(*session->controller, command_list)) {
                fill_acquisition_stim_result(
                    session,
                    out_result,
                    -122,
                    "failed to program RHS stimulation magnitudes before acquisition"
                );
                return out_result->return_code;
            }
        }

        command_length = registers.createCommandListRHSRegisterConfig(command_list, false);
        if (command_length <= 0) {
            fill_acquisition_stim_result(
                session,
                out_result,
                -123,
                "failed to create continuous RHS monitor command list"
            );
            return out_result->return_code;
        }
        session->controller->uploadCommandList(command_list, RHXController::AuxCmd1);
        session->controller->selectAuxCommandLength(
            RHXController::AuxCmd1,
            0,
            command_length - 1
        );

        for (int channel = 0; channel < 16; ++channel) {
            if ((channel_mask & (1 << channel)) == 0) continue;
            program_rhs_stim_sequence(*session->controller, stream, channel, *config);
        }
        }
        session->controller->enableAuxCommandsOnAllStreams();

        for (int trigger = 0; trigger < 8; ++trigger) {
            session->controller->setManualStimTrigger(trigger, false);
        }
        session->controller->setStimCmdMode(false);
        session->controller->resetSequencers();
        session->stim_config = *config;
        session->stim_configured = true;
        session->stim_active = false;

        std::ostringstream message;
        message << "stimulation preset configured for " << session->streams.size() << " acquisition streams"
                << ", pulses=" << config->pulses
                << "; output remains disabled until trigger";
        fill_acquisition_stim_result(session, out_result, 1, message.str());
        return 1;
    } catch (const std::exception& e) {
        session->stim_configured = false;
        session->stim_active = false;
        fill_acquisition_stim_result(
            session,
            out_result,
            -198,
            std::string("exception: ") + e.what()
        );
        return out_result->return_code;
    } catch (...) {
        session->stim_configured = false;
        session->stim_active = false;
        fill_acquisition_stim_result(session, out_result, -199, "unknown exception");
        return out_result->return_code;
    }
}

int rhs_acquisition_start(RhsAcquisitionSession* session, RhsAcquisitionInfo* out_info)
{
    reset_acquisition_info(out_info);
    if (!session || !out_info) return -101;
    try {
        if (session->running) {
            fill_acquisition_info(session, out_info, 1, "acquisition already running");
            return 1;
        }
        session->controller->setStimCmdMode(false);
        session->controller->setContinuousRunMode(true);
        session->controller->setMaxTimeStep(0xffffffffU);
        session->controller->run();
        session->running = true;
        fill_acquisition_info(
            session,
            out_info,
            1,
            session->stim_configured ?
                "continuous acquisition started with a preconfigured finite preset and stimulation output disabled" :
                "continuous acquisition started with stimulation disabled"
        );
        return 1;
    } catch (const std::exception& e) {
        fill_acquisition_info(session, out_info, -198, std::string("exception: ") + e.what());
        return -198;
    } catch (...) {
        fill_acquisition_info(session, out_info, -199, "unknown exception");
        return -199;
    }
}

int rhs_acquisition_trigger_stim(
    RhsAcquisitionSession* session,
    RhsAcquisitionStimResult* out_result
)
{
    reset_acquisition_stim_result(out_result);
    if (!session || !out_result) return -101;
    if (!session->running) {
        fill_acquisition_stim_result(session, out_result, -124, "acquisition is not running");
        return out_result->return_code;
    }
    if (!session->stim_configured) {
        fill_acquisition_stim_result(session, out_result, -125, "no stimulation preset is configured");
        return out_result->return_code;
    }

    try {
        enable_rhs_stim_routes(*session->controller, session->streams, session->target_stream, session->stim_config);
        session->controller->setStimCmdMode(true);
        session->controller->setManualStimTrigger(session->stim_config.trigger, true);
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
        session->controller->setManualStimTrigger(session->stim_config.trigger, false);
        session->stim_active = true;
        fill_acquisition_stim_result(
            session,
            out_result,
            1,
            "finite stimulation trigger asserted during continuous acquisition"
        );
        return 1;
    } catch (const std::exception& e) {
        try {
            session->controller->setManualStimTrigger(session->stim_config.trigger, false);
            session->controller->setStimCmdMode(false);
        } catch (...) {}
        session->stim_active = false;
        fill_acquisition_stim_result(
            session,
            out_result,
            -198,
            std::string("exception: ") + e.what()
        );
        return out_result->return_code;
    } catch (...) {
        try {
            session->controller->setManualStimTrigger(session->stim_config.trigger, false);
            session->controller->setStimCmdMode(false);
        } catch (...) {}
        session->stim_active = false;
        fill_acquisition_stim_result(session, out_result, -199, "unknown exception");
        return out_result->return_code;
    }
}

int rhs_acquisition_cancel_stim(
    RhsAcquisitionSession* session,
    RhsAcquisitionStimResult* out_result
)
{
    reset_acquisition_stim_result(out_result);
    if (!session || !out_result) return -101;
    try {
        const bool was_active = session->stim_active;
        session->controller->setStimCmdMode(false);
        session->controller->resetSequencers();
        for (int trigger = 0; trigger < 8; ++trigger) {
            session->controller->setManualStimTrigger(trigger, false);
        }
        for (int stream : session->streams) disable_rhs_stim_stream(*session->controller, stream);
        session->controller->setStimCmdMode(false);
        session->controller->resetSequencers();
        session->stim_active = false;
        fill_acquisition_stim_result(
            session,
            out_result,
            1,
            was_active ?
                "active stimulation cancelled while acquisition remains running" :
                "stimulation output forced disabled while acquisition remains available"
        );
        return 1;
    } catch (const std::exception& e) {
        fill_acquisition_stim_result(
            session,
            out_result,
            -198,
            std::string("exception: ") + e.what()
        );
        return out_result->return_code;
    } catch (...) {
        fill_acquisition_stim_result(session, out_result, -199, "unknown exception");
        return out_result->return_code;
    }
}

int rhs_acquisition_read(
    RhsAcquisitionSession* session,
    uint32_t* timestamps,
    size_t timestamp_capacity,
    uint16_t* amplifier_samples,
    size_t amplifier_capacity,
    uint16_t* ttl_in,
    size_t ttl_capacity,
    uint64_t* stim_on,
    size_t stim_on_capacity,
    uint64_t* stim_polarity,
    size_t stim_polarity_capacity,
    uint64_t* compliance_limit,
    size_t compliance_capacity,
    uint16_t* dc_amplifier_samples,
    size_t dc_amplifier_capacity,
    RhsAcquisitionReadResult* out_result
)
{
    reset_read_result(out_result);
    if (!session || !out_result) return -101;
    if (!session->running) {
        out_result->return_code = -110;
        set_read_message(out_result, "acquisition is not running");
        return out_result->return_code;
    }

    const size_t sample_count = static_cast<size_t>(session->samples_per_block) *
                                static_cast<size_t>(session->blocks_per_read);
    const size_t amplifier_count = sample_count * static_cast<size_t>(session->channel_count);
    if (!timestamps || !amplifier_samples || !ttl_in || !stim_on || !stim_polarity ||
        !compliance_limit || !dc_amplifier_samples ||
        timestamp_capacity < sample_count ||
        amplifier_capacity < amplifier_count ||
        ttl_capacity < sample_count ||
        stim_on_capacity < sample_count ||
        stim_polarity_capacity < sample_count ||
        compliance_capacity < sample_count ||
        dc_amplifier_capacity < amplifier_count) {
        out_result->return_code = -111;
        set_read_message(out_result, "output buffer capacity is too small");
        return out_result->return_code;
    }

    try {
        long bytes_read = session->controller->readDataBlocksRaw(
            session->blocks_per_read,
            session->raw_buffer.data()
        );
        out_result->bytes_read = static_cast<int>(bytes_read);
        out_result->pipe_read_error = session->controller->pipeReadError();
        if (bytes_read == 0) {
            out_result->return_code = 0;
            set_read_message(out_result, "no complete USB batch available");
            return 0;
        }

        const long expected_bytes = static_cast<long>(session->raw_buffer.size());
        if (bytes_read != expected_bytes) {
            out_result->return_code = -112;
            set_read_message(out_result, "USB pipe returned a partial acquisition batch");
            return out_result->return_code;
        }

        const int frame_bytes = session->bytes_per_block / session->samples_per_block;
        int invalid_headers = 0;
        for (int block = 0; block < session->blocks_per_read; ++block) {
            const int block_offset = block * session->bytes_per_block;
            for (int sample = 0; sample < session->samples_per_block; ++sample) {
                if (!RHXDataBlock::checkUsbHeader(
                        session->raw_buffer.data(),
                        block_offset + sample * frame_bytes,
                        ControllerStimRecord)) {
                    ++invalid_headers;
                }
            }
        }
        out_result->invalid_header_count = invalid_headers;
        if (invalid_headers != 0) {
            out_result->return_code = -113;
            set_read_message(out_result, "invalid Intan USB frame header detected");
            return out_result->return_code;
        }

        RHXDataBlock data_block(ControllerStimRecord, static_cast<int>(session->streams.size()));
        size_t sample_offset = 0;
        size_t amplifier_offset = 0;
        size_t dc_amplifier_offset = 0;
        for (int block = 0; block < session->blocks_per_read; ++block) {
            data_block.fillFromUsbBuffer(session->raw_buffer.data(), block);
            for (int sample = 0; sample < session->samples_per_block; ++sample) {
                timestamps[sample_offset] = data_block.timeStamp(sample);
                uint16_t ttl_mask = 0;
                uint64_t stim_on_mask = 0;
                uint64_t stim_polarity_mask = 0;
                uint64_t compliance_mask = 0;
                for (int bit = 0; bit < 16; ++bit) {
                    if (data_block.ttlIn(bit, sample)) ttl_mask |= static_cast<uint16_t>(1U << bit);
                }
                for (int channel = 0; channel < session->channel_count; ++channel) {
                    const int stream = channel / 16;
                    const int chip_channel = channel % 16;
                    if (data_block.stimOn(stream, chip_channel, sample)) {
                        stim_on_mask |= uint64_t(1) << channel;
                    }
                    if (data_block.stimPol(stream, chip_channel, sample)) {
                        stim_polarity_mask |= uint64_t(1) << channel;
                    }
                    if (data_block.complianceLimit(stream, chip_channel, sample)) {
                        compliance_mask |= uint64_t(1) << channel;
                    }
                    amplifier_samples[amplifier_offset++] = static_cast<uint16_t>(
                        data_block.amplifierData(stream, chip_channel, sample)
                    );
                    dc_amplifier_samples[dc_amplifier_offset++] = static_cast<uint16_t>(
                        data_block.dcAmplifierData(stream, chip_channel, sample)
                    );
                }
                ttl_in[sample_offset] = ttl_mask;
                stim_on[sample_offset] = stim_on_mask;
                stim_polarity[sample_offset] = stim_polarity_mask;
                compliance_limit[sample_offset] = compliance_mask;
                ++sample_offset;
            }
        }

        out_result->return_code = 1;
        out_result->block_count = session->blocks_per_read;
        out_result->samples_per_channel = static_cast<int>(sample_count);
        out_result->first_timestamp = timestamps[0];
        out_result->last_timestamp = timestamps[sample_count - 1];
        set_read_message(out_result, "acquisition batch read successfully");
        return 1;
    } catch (const std::exception& e) {
        out_result->return_code = -198;
        set_read_message(out_result, std::string("exception: ") + e.what());
        return out_result->return_code;
    } catch (...) {
        out_result->return_code = -199;
        set_read_message(out_result, "unknown exception");
        return out_result->return_code;
    }
}

int rhs_acquisition_stop(RhsAcquisitionSession* session, RhsAcquisitionInfo* out_info)
{
    reset_acquisition_info(out_info);
    if (!session || !out_info) return -101;
    try {
        session->controller->setStimCmdMode(false);
        for (int trigger = 0; trigger < 8; ++trigger) {
            session->controller->setManualStimTrigger(trigger, false);
        }
        session->controller->resetSequencers();
        session->controller->setContinuousRunMode(false);
        session->controller->setStimCmdMode(false);
        session->controller->setMaxTimeStep(0);

        for (int attempt = 0; attempt < 100 && session->controller->isRunning(); ++attempt) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
        if (session->controller->isRunning()) {
            session->controller->resetBoard();
        }
        session->controller->flush();
        session->running = false;
        session->stim_active = false;
        fill_acquisition_info(session, out_info, 1, "acquisition stopped and stimulation disabled");
        return 1;
    } catch (const std::exception& e) {
        session->running = false;
        session->stim_active = false;
        fill_acquisition_info(session, out_info, -198, std::string("exception: ") + e.what());
        return -198;
    } catch (...) {
        session->running = false;
        session->stim_active = false;
        fill_acquisition_info(session, out_info, -199, "unknown exception");
        return -199;
    }
}

void rhs_acquisition_close(RhsAcquisitionSession* session)
{
    if (!session) return;
    try {
        RhsAcquisitionInfo ignored;
        rhs_acquisition_stop(session, &ignored);
    } catch (...) {
    }
    delete session;
}

struct RhsImpedanceSession {
    RhsAcquisitionSession* acquisition = nullptr;
    rhs_impedance::Plan plan;
    int active_stream = -1;
    ~RhsImpedanceSession() { rhs_acquisition_close(acquisition); }
};

namespace {
void wait_impedance_run(RHXController& controller, int samples,
    RhsImpedanceCancelled cancelled, void* context)
{
    const auto deadline = std::chrono::steady_clock::now() +
        std::chrono::milliseconds(2000 + samples / 30);
    while (controller.isRunning()) {
        if (cancelled && cancelled(context)) throw std::runtime_error("impedance measurement cancelled");
        if (std::chrono::steady_clock::now() >= deadline) throw std::runtime_error("impedance finite-run timeout");
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
}

void restore_impedance(RhsImpedanceSession& session)
{
    if (session.active_stream < 0) return;
    auto& controller = *session.acquisition->controller;
    controller.setContinuousRunMode(false);
    controller.setMaxTimeStep(0);
    try { wait_impedance_run(controller, 0, nullptr, nullptr); }
    catch (...) { controller.resetBoard(); session.active_stream = -1; throw; }
    controller.flush();
    controller.enableAuxCommandsOnOneStream(session.active_stream);
    RHXRegisters registers(ControllerStimRecord, 30000.0);
    registers.enableZcheck(false);
    registers.setZcheckDacPower(false);
    std::vector<unsigned int> commands;
    int length = registers.createCommandListZcheckDac(commands, 0, 0);
    controller.uploadCommandList(commands, RHXController::AuxCmd1);
    controller.selectAuxCommandLength(RHXController::AuxCmd1, 0, length - 1);
    length = registers.createCommandListRHSRegisterConfig(commands, false);
    controller.uploadCommandList(commands, RHXController::AuxCmd3);
    controller.selectAuxCommandLength(RHXController::AuxCmd3, 0, length - 1);
    controller.setMaxTimeStep(256);
    controller.run();
    try { wait_impedance_run(controller, 256, nullptr, nullptr); }
    catch (...) { controller.resetBoard(); session.active_stream = -1; throw; }
    controller.flush();
    controller.setMaxTimeStep(0);
    session.active_stream = -1;
}
}

int rhs_impedance_open(const char* bitfile, const char* serial, int channels,
    double frequency, RhsImpedanceSession** out, RhsAcquisitionInfo* info)
{
    if (out) *out = nullptr;
    reset_acquisition_info(info);
    if (!out || !info) return -101;
    try {
        std::unique_ptr<RhsImpedanceSession> session(new RhsImpedanceSession());
        session->plan = rhs_impedance::plan(frequency);
        const int code = rhs_acquisition_open_channels(bitfile, serial, 1, channels, &session->acquisition, info);
        if (code < 0) return code;
        for (int dac = 0; dac < 8; ++dac) session->acquisition->controller->enableDac(dac, false);
        *out = session.release();
        return 1;
    } catch (const std::exception& error) {
        set_acquisition_message(info, error.what());
        info->return_code = -198;
        return -198;
    } catch (...) { set_acquisition_message(info, "unknown impedance open error"); return -199; }
}

int rhs_impedance_measure(RhsImpedanceSession* session, int index,
    RhsImpedanceCancelled cancelled, void* context, RhsImpedanceResult* result)
{
    if (!result) return -101;
    *result = RhsImpedanceResult{};
    result->return_code = -101;
    if (!session || index < 0 || index >= session->acquisition->channel_count) return -101;
    try {
        auto& acquisition = *session->acquisition;
        auto& controller = *acquisition.controller;
        const int stream = index / 16, channel = index % 16;
        const auto& p = session->plan;
        session->active_stream = acquisition.streams[stream];
        controller.enableAuxCommandsOnOneStream(session->active_stream);
        controller.setStimCmdMode(false);
        controller.setContinuousRunMode(false);
        RHXRegisters registers(ControllerStimRecord, 30000.0);
        registers.enableDsp(false);
        registers.setLowerBandwidth(0.1, 0);
        const double upper = registers.setUpperBandwidth(7500.0);
        registers.enableZcheck(true);
        registers.setZcheckChannel(channel);
        std::array<rhs_impedance::Polar, 3> voltages;
        const RHXRegisters::ZcheckCs scales[] = {
            RHXRegisters::ZcheckCs100fF, RHXRegisters::ZcheckCs1pF, RHXRegisters::ZcheckCs10pF};
        for (int range = 0; range < 3; ++range) {
            if (cancelled && cancelled(context)) throw std::runtime_error("impedance measurement cancelled");
            std::vector<unsigned int> commands;
            int length = registers.createCommandListZcheckDac(commands, p.frequency, 128.0);
            controller.uploadCommandList(commands, RHXController::AuxCmd1);
            controller.selectAuxCommandLength(RHXController::AuxCmd1, 0, length - 1);
            registers.setZcheckScale(scales[range]);
            length = registers.createCommandListRHSRegisterConfig(commands, false);
            controller.uploadCommandList(commands, RHXController::AuxCmd3);
            controller.selectAuxCommandLength(RHXController::AuxCmd3, 0, length - 1);
            controller.setMaxTimeStep(p.blocks * 128);
            controller.run();
            wait_impedance_run(controller, p.blocks * 128, cancelled, context);
            std::vector<uint8_t> raw(static_cast<size_t>(acquisition.bytes_per_block) * p.blocks);
            const long bytes = controller.readDataBlocksRaw(p.blocks, raw.data());
            if (bytes != static_cast<long>(raw.size()) || controller.pipeReadError() != 0)
                throw std::runtime_error("impedance USB batch incomplete");
            RHXDataBlock block(ControllerStimRecord, static_cast<int>(acquisition.streams.size()));
            std::vector<double> waveform;
            waveform.reserve(p.blocks * 128);
            uint32_t previous = 0;
            bool first = true;
            for (int b = 0; b < p.blocks; ++b) {
                for (int t = 0; t < 128; ++t) {
                    const int offset = b * acquisition.bytes_per_block + t * acquisition.bytes_per_block / 128;
                    if (!RHXDataBlock::checkUsbHeader(raw.data(), offset, ControllerStimRecord))
                        throw std::runtime_error("invalid impedance USB frame header");
                }
                block.fillFromUsbBuffer(raw.data(), b);
                for (int t = 0; t < 128; ++t) {
                    const uint32_t timestamp = block.timeStamp(t);
                    if (!first && timestamp != uint32_t(previous + 1)) throw std::runtime_error("impedance timestamp discontinuity");
                    first = false; previous = timestamp;
                    if (block.stimOn(stream, channel, t)) throw std::runtime_error("unexpected stimulation during impedance test");
                    waveform.push_back(0.195 * (block.amplifierData(stream, channel, t) - 32768));
                }
            }
            voltages[range] = rhs_impedance::correlate(waveform, p);
        }
        const auto measured = rhs_impedance::estimate(voltages, p, upper);
        result->stream = session->active_stream;
        result->channel = channel;
        result->frequency_hz = p.frequency;
        result->magnitude_ohms = measured.magnitude;
        result->phase_degrees = measured.phase;
        result->quality = measured.quality;
        restore_impedance(*session);
        result->return_code = 1;
        return 1;
    } catch (const std::exception& error) {
        std::string message = error.what();
        try { restore_impedance(*session); }
        catch (const std::exception& cleanup) { message += std::string("; cleanup failed: ") + cleanup.what(); }
        std::copy_n(message.data(), std::min(message.size(), sizeof(result->message) - 1), result->message);
        result->return_code = message == "impedance measurement cancelled" ? -2 : -198;
        return result->return_code;
    } catch (...) {
        try { restore_impedance(*session); } catch (...) {}
        result->return_code = -199;
        return -199;
    }
}

void rhs_impedance_close(RhsImpedanceSession* session)
{
    if (!session) return;
    try { restore_impedance(*session); } catch (...) {}
    delete session;
}
