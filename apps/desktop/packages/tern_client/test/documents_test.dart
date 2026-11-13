import 'dart:convert';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

void main() {
  test('decodes a document, its text and its revisions', () {
    final d = DocumentContent.fromJson(
      jsonDecode(
            '{"id":"d_1","title":"Plan","kind":"markdown","path":"documents/plan.md",'
            '"workspace_id":"default","rev":2,"bytes":12,"tainted":true,'
            '"agent_id":"tern","session_id":"s","visibility":"private",'
            '"created_at":1,"updated_at":2000,"content":"# Plan"}',
          )
          as Json,
    );
    expect(d.meta.id, 'd_1');
    expect(d.meta.rev, 2);
    expect(d.meta.tainted, isTrue);
    expect(d.content, '# Plan');
    expect(d.meta.updatedAt, DateTime.fromMillisecondsSinceEpoch(2000));

    final r = DocumentRevision.fromJson(
      jsonDecode(
            '{"rev":2,"parent_rev":1,"author_kind":"agent","author_id":"tern",'
            '"added":3,"removed":1,"bytes":12,"tainted":false,"rebased":true,'
            '"created_at":5}',
          )
          as Json,
    );
    expect(r.authorKind, 'agent');
    expect(r.rebased, isTrue);
    expect((r.added, r.removed), (3, 1));
    expect(r.content, isNull);
  });

  test('a minimal document row still decodes', () {
    final m = DocumentMeta.fromJson({'id': 'd', 'title': 'T'});
    expect(m.kind, 'markdown');
    expect(m.rev, 1);
    expect(m.tainted, isFalse);
  });
}
