function test_neurox_export(directory)
% Verify a complete 64-channel fixture written by the native export tests.
d = read_neurox(directory);
assert(isequal(size(d.adc), [4, 64]));
assert(strcmp(d.channels{1}, 'B-00'));
assert(strcmp(d.channels{33}, 'C-00'));
assert(isequal(d.sample_index, uint64([1000; 1001; 1100; 1101])));
assert(d.adc(1, 2) == 32769);
assert(abs((double(d.adc(1, 2)) - 32768) * .195 - .195) < 1e-12);
assert(isequal(d.ttl_in, uint16([1; 2; 0; 0])));
assert(d.blocks.flags(2) == 3);
assert(d.blocks.ttl_present(2) == 0);
csv = readtable(fullfile(directory, 'samples.csv'));
assert(isequal(size(csv), [4, 68]));
assert(abs(csv{2, 7} - (double(d.adc(2, 3)) - 32768) * .195) < 1e-9);
disp('PASS: MATLAB reads native 64-channel binary/CSV export, units, TTL and time gaps');
end
