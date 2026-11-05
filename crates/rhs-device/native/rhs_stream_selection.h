#pragma once

#include "rhs_scan_bridge.h"
#include "rhxglobals.h"

#include <array>
#include <string>
#include <vector>

inline bool select_rhs_acquisition_streams(const RhsScanResult& scan, int channel_count,
                                           std::vector<int>& selected, std::string& error)
{
    selected.clear();
    error.clear();
    if (scan.chip_count < 0 || scan.chip_count > RHS_SCAN_MAX_RESULTS ||
        (channel_count != 16 && channel_count != 64)) {
        error = "invalid chip scan or unsupported acquisition channel count";
        return false;
    }
    std::array<bool, RHS_SCAN_MAX_RESULTS> seen{};
    int first_stream = -1;
    for (int index = 0; index < scan.chip_count; ++index) {
        if (scan.chip_type[index] != RHS2116Chip) continue;
        const int stream = scan.command_stream[index];
        const int port = scan.port_index[index];
        if (stream < 0 || stream >= RHS_SCAN_MAX_RESULTS || port != stream / 2 || seen[stream]) {
            error = "RHS2116 scan contains an invalid, duplicate or mismatched port/stream";
            return false;
        }
        seen[stream] = true;
        if (first_stream < 0) first_stream = stream;
    }
    if (channel_count == 16 && first_stream >= 0) {
        selected.push_back(first_stream);
        return true;
    }
    if (channel_count == 64) {
        // Keep the probe's B/C wiring even when other ports have connected chips.
        for (int stream : {2, 3, 4, 5}) {
            if (!seen[stream]) {
                error = "MEA 64 requires RHS2116 streams B2/B3/C4/C5; missing streams are never substituted";
                return false;
            }
        }
        selected = {2, 3, 4, 5};
        return true;
    }
    error = "no RHS2116 stream is available for acquisition";
    return false;
}
