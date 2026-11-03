import 'dart:convert';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

Json j(String s) => jsonDecode(s) as Json;

void main() {
  test('decodes provider statuses and verifications', () {
    final p = ProviderStatus.fromJson(
      j(
        '{"name":"local","kind":"openai_compat","base_url":"http://x/v1",'
        '"secret":null,"key_source":"none","key_hint":null,'
        '"error":{"kind":"unreachable","detail":"refused"},'
        '"message":"Couldn\'t reach x.","is_default":true}',
      ),
    );
    expect(p.kind, ProviderKind.openaiCompat);
    expect(p.error?.kind, 'unreachable');
    expect(p.error?.detail, 'refused');
    expect(p.isDefault, isTrue);

    final v = Verified.fromJson(
      j(
        '{"ok":true,"name":"anthropic","error":null,"message":null,'
        '"warnings":["w"],"suggested_base_url":null,"key_hint":"sk-ant…a1B2",'
        '"models":[{"id":"m","name":"M","context_window":200000}]}',
      ),
    );
    expect(v.models.single.contextWindow, 200000);
    expect(v.warnings, ['w']);
    expect(
      Verified.fromJson(
        j(
          '{"ok":false,"name":"a","error":{"kind":"unauthorized"},"models":[]}',
        ),
      ).error?.kind,
      'unauthorized',
    );
  });

  test('drafts leave out blanks', () {
    expect(
      const ProviderDraft(
        kind: ProviderKind.anthropic,
        name: ' ',
        key: ' sk ',
      ).toJson(),
      {'kind': 'anthropic', 'key': 'sk', 'use_env_key': false},
    );
  });

  test('decodes change events and setup_needed', () {
    expect(
      TernEvent.fromJson(j('{"providers_changed":{}}')),
      isA<ProvidersChanged>(),
    );
    expect(
      TernEvent.fromJson(j('{"models_changed":{"provider":"x"}}')),
      isA<ModelsChanged>().having((e) => e.provider, 'provider', 'x'),
    );
    expect(
      Info.fromJson(j('{"tools":[],"setup_needed":true}')).setupNeeded,
      true,
    );
    expect(Info.fromJson(j('{"tools":[]}')).setupNeeded, false);
  });
}
