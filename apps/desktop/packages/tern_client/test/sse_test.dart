import 'dart:convert';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

void main() {
  test('splits events, skips keep-alives, joins multi-line data', () async {
    final raw = ['data: {"a":1}\n\n', ':\n\n', 'data: one\ndata: two\n', '\n'];
    final out = await Stream.fromIterable(
      raw.map<List<int>>(utf8.encode),
    ).transform(const SseDecoder()).toList();
    expect(out, ['{"a":1}', 'one\ntwo']);
  });

  test('handles chunks split mid-line', () async {
    final out = await Stream.fromIterable(
      ['da', 'ta: {"x"', ':2}\n', '\n'].map<List<int>>(utf8.encode),
    ).transform(const SseDecoder()).toList();
    expect(out, ['{"x":2}']);
  });
}
