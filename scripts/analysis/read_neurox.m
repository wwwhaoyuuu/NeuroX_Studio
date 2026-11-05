function data = read_neurox(directory)
% Read the raw binary NeuroX analysis export. No third-party toolbox required.
if isfile(fullfile(directory, 'INCOMPLETE.txt'))
    error('NeuroX:Incomplete', 'Export is incomplete');
end
p = jsondecode(fileread(fullfile(directory, 'parameters.json')));
if p.schemaVersion ~= 1 || ~p.complete
    error('NeuroX:Format', 'Unsupported or incomplete export');
end
data.adc = read_binary(directory, 'samples.u16le', 'uint16=>uint16', ...
    2, p.samplesPerChannel * p.channelCount);
data.adc = reshape(data.adc, p.channelCount, p.samplesPerChannel).';
data.sample_index = read_binary(directory, 'sample_index.u64le', ...
    'uint64=>uint64', 8, p.samplesPerChannel);
data.ttl_in = read_binary(directory, 'ttl_in.u16le', ...
    'uint16=>uint16', 2, p.samplesPerChannel);
data.blocks = readtable(fullfile(directory, 'blocks.csv'));
data.channels = {p.channels.label};
data.parameters = p;
% Convert only selected rows/channels to avoid allocating all data as double:
% uv = (double(data.adc(1:30000, 1)) - p.adcZero) * p.microvoltsPerAdc;
end

function values = read_binary(directory, name, precision, bytes, count)
path = fullfile(directory, name);
info = dir(path);
if isempty(info) || info.bytes ~= bytes * count
    error('NeuroX:Size', 'Unexpected file size: %s', name);
end
file = fopen(path, 'r', 'ieee-le');
if file < 0, error('NeuroX:File', 'Cannot open %s', name); end
cleanup = onCleanup(@() fclose(file));
values = fread(file, count, precision);
end
