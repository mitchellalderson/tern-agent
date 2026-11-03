import 'dart:async';
import 'dart:convert';

/// Splits a Server-Sent Events byte stream into the `data` of each event.
/// Comment lines (keep-alives) and other fields are ignored.
class SseDecoder extends StreamTransformerBase<List<int>, String> {
  const SseDecoder();

  @override
  Stream<String> bind(Stream<List<int>> stream) async* {
    final data = <String>[];
    await for (final line
        in stream.transform(utf8.decoder).transform(const LineSplitter())) {
      if (line.isEmpty) {
        if (data.isNotEmpty) {
          yield data.join('\n');
          data.clear();
        }
      } else if (line.startsWith('data:')) {
        var value = line.substring(5);
        if (value.startsWith(' ')) value = value.substring(1);
        data.add(value);
      }
    }
    if (data.isNotEmpty) yield data.join('\n');
  }
}
