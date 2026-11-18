import 'dart:convert';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

Json j(String s) => jsonDecode(s) as Json;

void main() {
  test('decodes the local status, the plan and the install progress', () {
    final s = LocalStatus.fromJson(
      j(
        '{"engine":"ollama","installed":true,"running":false,"version":null,'
        '"too_old":false,"managed":false,"url":"http://127.0.0.1:11434",'
        '"models_dir":"/m","disk_free":5,"ram_bytes":17179869184,'
        '"context":4096,"context_source":"estimate","warnings":["w"],'
        '"install_page":"https://ollama.com/download",'
        '"install":{"source":"s","destination":"d","summary":"says",'
        '"available":true},'
        '"installing":{"state":"working","phase":"downloading",'
        '"completed":25,"total":100}}',
      ),
    );
    expect(s.installed, isTrue);
    expect(s.running, isFalse);
    expect(s.context, 4096);
    expect(s.warnings, ['w']);
    expect(s.install.available, isTrue);
    expect(s.install.summary, 'says');
    expect(s.installing.working, isTrue);
    expect(s.installing.fraction, 0.25);
  });

  test('an older daemon\'s short answer still decodes', () {
    final s = LocalStatus.fromJson(j('{}'));
    expect(s.installed, isFalse);
    expect(s.installing.state, 'idle');
    expect(s.installing.fraction, isNull);
  });

  test('decodes a local model and the change event', () {
    final m = LocalModel.fromJson(
      j(
        '{"name":"qwen3:8b","size":5,"digest":"d","parameter_size":"8B",'
        '"tools":true,"thinking":false,"max_context":40960,"loaded":true}',
      ),
    );
    expect(m.name, 'qwen3:8b');
    expect(m.tools, isTrue);
    expect(m.maxContext, 40960);
    expect(m.loaded, isTrue);
    expect(TernEvent.fromJson(j('{"local_changed":{}}')), isA<LocalChanged>());
  });
}
