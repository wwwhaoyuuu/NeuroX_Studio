#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum { RHS_SCAN_MAX_RESULTS = 8 };

typedef struct RhsScanResult {
    int return_code;
    int chip_count;
    int chip_type[RHS_SCAN_MAX_RESULTS];
    int port_index[RHS_SCAN_MAX_RESULTS];
    int command_stream[RHS_SCAN_MAX_RESULTS];
    int channels_on_port[4];
    int pipe_read_error;
    char message[512];
} RhsScanResult;

int rhs_scan_connected_chips(const char* bitfile_path, const char* serial, RhsScanResult* out);

typedef struct RhsStimConfig {
    int channel;
    int trigger;
    int pulses;
    int magnitude_steps;
    int stim_step_size;
    int phase_steps;
    int interphase_steps;
    int period_steps;
} RhsStimConfig;

typedef struct RhsStimSession RhsStimSession;

int rhs_stim_open(
    const char* bitfile_path,
    const char* serial,
    RhsStimSession** out_session,
    RhsScanResult* out
);

int rhs_stim_start(
    RhsStimSession* session,
    const RhsStimConfig* config,
    RhsScanResult* out
);

int rhs_stim_poll(RhsStimSession* session, RhsScanResult* out);

int rhs_stim_cancel(RhsStimSession* session, RhsScanResult* out);

void rhs_stim_close(RhsStimSession* session);

int rhs_run_bounded_stim(
    const char* bitfile_path,
    const char* serial,
    const RhsStimConfig* config,
    RhsScanResult* out
);

typedef struct RhsAcquisitionSession RhsAcquisitionSession;

typedef struct RhsAcquisitionInfo {
    int return_code;
    int sample_rate_hz;
    int channel_count;
    int samples_per_block;
    int blocks_per_read;
    int target_stream;
    int target_port;
    int bytes_per_block;
    int pipe_read_error;
    char device_serial[64];
    char message[512];
} RhsAcquisitionInfo;

typedef struct RhsAcquisitionReadResult {
    int return_code;
    int block_count;
    int samples_per_channel;
    int invalid_header_count;
    uint32_t first_timestamp;
    uint32_t last_timestamp;
    int pipe_read_error;
    int bytes_read;
    char message[512];
} RhsAcquisitionReadResult;

typedef struct RhsAcquisitionStimResult {
    int return_code;
    int active;
    int pipe_read_error;
    char message[512];
} RhsAcquisitionStimResult;

int rhs_acquisition_open(
    const char* bitfile_path,
    const char* serial,
    int blocks_per_read,
    RhsAcquisitionSession** out_session,
    RhsAcquisitionInfo* out_info
);

int rhs_acquisition_start(RhsAcquisitionSession* session, RhsAcquisitionInfo* out_info);

int rhs_acquisition_configure_stim(
    RhsAcquisitionSession* session,
    const RhsStimConfig* config,
    RhsAcquisitionStimResult* out_result
);

int rhs_acquisition_trigger_stim(
    RhsAcquisitionSession* session,
    RhsAcquisitionStimResult* out_result
);

int rhs_acquisition_cancel_stim(
    RhsAcquisitionSession* session,
    RhsAcquisitionStimResult* out_result
);

int rhs_acquisition_read(
    RhsAcquisitionSession* session,
    uint32_t* timestamps,
    size_t timestamp_capacity,
    uint16_t* amplifier_samples,
    size_t amplifier_capacity,
    uint16_t* ttl_in,
    size_t ttl_capacity,
    uint16_t* stim_on,
    size_t stim_on_capacity,
    uint16_t* stim_polarity,
    size_t stim_polarity_capacity,
    uint16_t* compliance_limit,
    size_t compliance_capacity,
    uint16_t* dc_amplifier_samples,
    size_t dc_amplifier_capacity,
    RhsAcquisitionReadResult* out_result
);

int rhs_acquisition_stop(RhsAcquisitionSession* session, RhsAcquisitionInfo* out_info);

void rhs_acquisition_close(RhsAcquisitionSession* session);

#ifdef __cplusplus
}
#endif
