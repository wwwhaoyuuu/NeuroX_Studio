// Impedance estimation adapted from Intan RHX ImpedanceReader (2020-2025),
// GPL-3.0-or-later. See vendor/intan-rhythmstim-api-230822/COPYING.
#pragma once
#include <algorithm>
#include <array>
#include <cmath>
#include <stdexcept>
#include <vector>

namespace rhs_impedance {
constexpr double pi = 3.14159265358979323846;
struct Plan { int period; int periods; int blocks; double frequency; };
struct Polar { double magnitude; double phase; };
struct Result { double magnitude; double phase; int quality; };

inline Plan plan(double requested) {
    if (!std::isfinite(requested) || requested < 10.0 || requested > 2000.0)
        throw std::invalid_argument("impedance frequency must be 10..2000 Hz");
    const int period = static_cast<int>(std::round(30000.0 / requested));
    const double actual = 30000.0 / period;
    const int periods = std::max(5, static_cast<int>(std::round(0.020 * actual)));
    return {period, periods, std::max(2, static_cast<int>(std::ceil((periods + 2.0) * period / 128.0))), actual};
}

inline Polar correlate(const std::vector<double>& waveform, const Plan& p) {
    const int length = p.periods * p.period;
    if (static_cast<int>(waveform.size()) < length + p.period)
        throw std::runtime_error("insufficient impedance samples");
    const int start = ((static_cast<int>(waveform.size()) - length) / p.period) * p.period;
    double real = 0.0, imag = 0.0;
    for (int t = start; t < start + length; ++t) {
        real += waveform[t] * std::cos(2.0 * pi * t / p.period);
        imag -= waveform[t] * std::sin(2.0 * pi * t / p.period);
    }
    return {2.0 * std::hypot(real, imag) / length, std::atan2(imag, real)};
}

inline Result estimate(const std::array<Polar, 3>& v, const Plan& p, double upper) {
    const double saturation = p.frequency < 0.2 * upper ? 5000.0 :
        5000.0 / std::sqrt(1.0 + std::pow(3.3333 * p.frequency / upper, 4.0));
    int range = v[2].magnitude < saturation ? 2 : v[1].magnitude < saturation ? 1 : 0;
    if (range == 2 && v[1].magnitude > 0.2 * v[2].magnitude) range = 1;
    const double capacitances[] = {0.1e-12, 1.0e-12, 10.0e-12};
    const double current = 2.0 * pi * p.frequency * (128.0 * 1.225 / 256.0) * capacitances[range];
    const double relative = p.frequency / 30000.0;
    const double magnitude = 1e-6 * v[range].magnitude / current * (18.0 * relative * relative + 1.0);
    const double phase = v[range].phase + 2.0 * pi * 3.0 / p.period;
    const double r = magnitude * std::cos(phase), x = magnitude * std::sin(phase);
    const double cap = 2.0 * pi * p.frequency * 12e-12;
    const double term = cap * (r * r + x * x);
    const double denominator = cap * term + 2.0 * cap * x + 1.0;
    if (denominator < 1e-12) return {0, 0, 3};
    const double true_r = r / denominator, true_x = (x + term) / denominator;
    const double corrected = 1.1 * std::hypot(true_r, true_x);
    if (!std::isfinite(corrected)) return {0, 0, 3};
    return {corrected, std::atan2(true_x, true_r) * 180.0 / pi,
        v[range].magnitude >= saturation ? 1 : v[range].magnitude < 5.0 ? 2 : 0};
}
}
