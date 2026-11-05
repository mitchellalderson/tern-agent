import 'dart:convert';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

Json j(String s) => jsonDecode(s) as Json;

void main() {
  test('decodes presets, labels, suggestions and the preset in a draft', () {
    final p = ProviderPreset.fromJson(
      j(
        '{"id":"openrouter","label":"OpenRouter","blurb":"b","kind":"openai_compat",'
        '"base_url":"https://openrouter.ai/api/v1","name":"openrouter",'
        '"key_url":"https://openrouter.ai/keys","key_hint":"h","needs_key":true,'
        '"local":false,"suggested_models":["a/b"]}',
      ),
    );
    expect(p.kind, ProviderKind.openaiCompat);
    expect(p.baseUrl, 'https://openrouter.ai/api/v1');
    expect(p.suggestedModels, ['a/b']);
    expect(p.isOther, isFalse);

    final s = ProviderStatus.fromJson(
      j(
        '{"name":"openrouter","kind":"openai_compat","base_url":"u",'
        '"key_source":"file","preset":"openrouter","label":"OpenRouter",'
        '"is_default":false}',
      ),
    );
    expect(s.preset, 'openrouter');
    expect(s.label, 'OpenRouter');
    // Older cores send no label: fall back to the name.
    expect(
      ProviderStatus.fromJson(
        j('{"name":"x","kind":"anthropic","key_source":"none"}'),
      ).label,
      'x',
    );
    expect(
      ProviderModels.fromJson(
        j('{"provider":"p","models":[],"suggested":["m"]}'),
      ).suggested,
      ['m'],
    );
    expect(const ProviderDraft(preset: 'groq').toJson()['preset'], 'groq');
  });

  test('explains call errors and keeps unknown text as is', () {
    final f = friendlyCallError(
      'HTTP 404: {"error":{"message":"Not Found","code":404}}',
      'OpenRouter',
    )!;
    expect(f.code, 'not_found');
    expect(
      f.message,
      "OpenRouter couldn't find that endpoint or model. Check the model name.",
    );
    expect(f.fixInModels, isTrue);
    expect(
      friendlyCallError('HTTP 404: No endpoints found for x/y', 'O')!.message,
      contains('find that model'),
    );
    expect(friendlyCallError('HTTP 401: nope', 'X')!.code, 'unauthorized');
    expect(friendlyCallError('HTTP 429: slow', 'X')!.fixInModels, isFalse);
    expect(friendlyCallError('HTTP 503: down', 'X')!.code, 'server');
    expect(friendlyCallError('network error: dns', 'X')!.code, 'unreachable');
    expect(friendlyCallError('HTTP 400: bad', 'X'), isNull);
    expect(friendlyCallError('tool failed', 'X'), isNull);
  });
}
