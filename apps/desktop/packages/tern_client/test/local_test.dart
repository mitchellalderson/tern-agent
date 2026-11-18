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

  test('decodes the catalog', () {
    final c = LocalCatalog.fromJson(
      j(
        '{"ram_bytes":17179869184,"setting_up":true,"error":null,"tiers":['
        '{"tier":"agent","model":"qwen3.5:9b","size_gb":6.6,"blurb":"b",'
        '"note":null,"installed":true,"current":"qwen3.5:9b"},'
        '{"tier":"decision","model":"granite4.2:3b","size_gb":2.0,'
        '"blurb":"d","note":"n","installed":false,"current":null}]}',
      ),
    );
    expect(c.settingUp, isTrue);
    expect(c.tiers.first.tier, LocalTier.agent);
    expect(c.tiers.first.inUse, isTrue);
    expect(c.tiers.last.note, 'n');
    expect(LocalTier.fast.needsTools, isFalse);
    expect(LocalTier.decision.needsTools, isTrue);
  });
}
