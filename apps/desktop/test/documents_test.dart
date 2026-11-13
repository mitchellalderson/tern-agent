// The Documents panel (docs/documents-design.md): tabs for several
// documents, safe Markdown, the "from the web" mark, history, and following
// the agent's writes.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/documents.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/documents/document_view.dart';
import 'package:tern_desktop/ui/documents/documents_panel.dart';
import 'package:tern_desktop/ui/documents/safe_markdown.dart';

final _t = DateTime(2026, 11, 10, 9);

DocumentMeta _meta(
  String id,
  String title, {
  int rev = 1,
  bool tainted = false,
  String kind = 'markdown',
}) => DocumentMeta(
  id: id,
  title: title,
  kind: kind,
  path: 'documents/$id.md',
  rev: rev,
  updatedAt: _t,
  tainted: tainted,
);

class _Api implements DocumentsApi {
  final texts = <String, String>{};
  final metas = <String, DocumentMeta>{};
  final revs = <String, List<DocumentRevision>>{};
  final reviewed = <String>[];

  void put(DocumentMeta m, String text, [List<DocumentRevision>? r]) {
    metas[m.id] = m;
    texts[m.id] = text;
    revs[m.id] =
        r ??
        [
          DocumentRevision(
            rev: m.rev,
            authorKind: 'agent',
            authorId: 'research',
            createdAt: _t,
          ),
        ];
  }

  @override
  Future<List<DocumentMeta>> list() async => metas.values.toList();
  @override
  Future<DocumentContent> get(String id) async =>
      DocumentContent(meta: metas[id]!, content: texts[id]!);
  @override
  Future<List<DocumentRevision>> revisions(String id) async => revs[id]!;
  @override
  Future<DocumentRevision> revision(String id, int rev) async {
    final r = revs[id]!.firstWhere((r) => r.rev == rev);
    return DocumentRevision(
      rev: r.rev,
      authorKind: r.authorKind,
      authorId: r.authorId,
      createdAt: r.createdAt,
      content: 'OLD TEXT of rev $rev',
    );
  }

  @override
  Future<void> markReviewed(String id) async {
    reviewed.add(id);
    metas[id] = _meta(id, metas[id]!.title, rev: metas[id]!.rev);
  }
}

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: SizedBox(width: 520, height: 700, child: child)),
);

Widget _panel(DocumentsState docs, {Future<void> Function(String)? open}) =>
    _app(
      DocumentsPanel(
        docs: docs,
        mode: PaneMode.documents,
        onMode: (_) {},
        onClose: () {},
        onOpenLink: open,
      ),
    );

/// Selectable Markdown draws through the editor, so a link is tapped by
/// firing the recogniser on its span.
void _tapLink(WidgetTester tester, String text) {
  void walk(InlineSpan span, List<TextSpan> out) {
    if (span is TextSpan) {
      if (span.text == text && span.recognizer != null) out.add(span);
      for (final c in span.children ?? const <InlineSpan>[]) {
        walk(c, out);
      }
    }
  }

  final found = <TextSpan>[];
  for (final w in tester.widgetList<SelectableText>(
    find.byType(SelectableText),
  )) {
    if (w.textSpan != null) walk(w.textSpan!, found);
  }
  expect(found, isNotEmpty, reason: 'no link span "$text"');
  (found.first.recognizer! as dynamic).onTap!();
}

void main() {
  testWidgets('several documents are tabs; switching shows each one', (
    tester,
  ) async {
    final api = _Api()
      ..put(_meta('a', 'Weekly report'), '# Weekly report\n\nRevenue grew.')
      ..put(_meta('b', 'Plan'), '# The plan\n\nStep one.');
    final docs = DocumentsState(api);
    await docs.refresh();
    await tester.pumpWidget(_panel(docs));
    // With nothing open: the library lists both.
    expect(find.text('Weekly report'), findsOneWidget);
    expect(find.text('Plan'), findsOneWidget);

    await docs.openDoc('a');
    await docs.openDoc('b');
    await tester.pumpAndSettle();
    expect(find.text('Step one.'), findsOneWidget);
    expect(find.text('Revenue grew.'), findsNothing);

    docs.select('a');
    await tester.pumpAndSettle();
    expect(find.text('Revenue grew.'), findsOneWidget);

    docs.close('a');
    await tester.pumpAndSettle();
    expect(docs.open, ['b']);
    expect(docs.selected, 'b');
  });

  testWidgets('markdown never renders raw html or fetches an image', (
    tester,
  ) async {
    await tester.pumpWidget(
      _app(
        SingleChildScrollView(
          child: SafeMarkdown(
            text:
                'Hello <script>alert(1)</script> world\n\n'
                '<img src="https://evil.example/x.png" onerror="x()">\n\n'
                '![chart](https://tracker.example/pixel.png)\n',
          ),
        ),
      ),
    );
    final texts = tester
        .widgetList<RichText>(find.byType(RichText))
        .map((r) => r.text.toPlainText())
        .join('\n');
    expect(texts, isNot(contains('<script>')));
    expect(texts, isNot(contains('onerror')));
    // The image is a placeholder naming the host, never an Image widget.
    expect(find.byType(Image), findsNothing);
    expect(find.textContaining('tracker.example'), findsOneWidget);
    expect(find.textContaining('not loaded'), findsOneWidget);
  });

  testWidgets('a link shows its full address and opens only when confirmed', (
    tester,
  ) async {
    final opened = <String>[];
    await tester.pumpWidget(
      _app(
        SafeMarkdown(
          text: '[the docs](https://example.com/a?b=1) and [bad](javascript:alert(1))',
          onOpenLink: (u) async => opened.add(u),
        ),
      ),
    );
    _tapLink(tester, 'the docs');
    await tester.pumpAndSettle();
    expect(find.text('Open this link?'), findsOneWidget);
    expect(find.text('https://example.com/a?b=1'), findsOneWidget);
    expect(opened, isEmpty);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(opened, isEmpty);

    _tapLink(tester, 'the docs');
    await tester.pumpAndSettle();
    await tester.tap(find.text('Open'));
    await tester.pumpAndSettle();
    expect(opened, ['https://example.com/a?b=1']);

    // A script link is inert: no dialog at all.
    _tapLink(tester, 'bad');
    await tester.pumpAndSettle();
    expect(find.text('Open this link?'), findsNothing);
    expect(opened, hasLength(1));
  });

  testWidgets('a document from the web is marked, and can be reviewed', (
    tester,
  ) async {
    final api = _Api()..put(_meta('w', 'Scraped', tainted: true), 'text');
    final docs = DocumentsState(api);
    await docs.refresh();
    await docs.openDoc('w');
    await tester.pumpWidget(_panel(docs));
    await tester.pumpAndSettle();
    expect(find.text('From the web, not reviewed'), findsOneWidget);
    await tester.tap(find.text('Mark as reviewed'));
    await tester.pumpAndSettle();
    expect(api.reviewed, ['w']);
    expect(find.text('From the web, not reviewed'), findsNothing);
  });

  testWidgets('history lists revisions and shows an older one read-only', (
    tester,
  ) async {
    final api = _Api()
      ..put(_meta('a', 'Report', rev: 2), 'NEW TEXT', [
        DocumentRevision(
          rev: 2,
          authorKind: 'agent',
          authorId: 'research',
          createdAt: _t,
          added: 3,
          removed: 1,
          summary: 'added Q3',
        ),
        DocumentRevision(
          rev: 1,
          authorKind: 'external',
          authorId: 'file',
          createdAt: _t,
        ),
      ]);
    final docs = DocumentsState(api);
    await docs.refresh();
    await docs.openDoc('a');
    await tester.pumpWidget(_panel(docs));
    await tester.pumpAndSettle();
    expect(find.text('NEW TEXT'), findsOneWidget);

    await tester.tap(find.text('History'));
    await tester.pumpAndSettle();
    expect(find.textContaining('+3 −1'), findsOneWidget);
    expect(find.text('Edited outside tern'), findsOneWidget);
    expect(find.text('current'), findsOneWidget);

    await tester.tap(find.text('Edited outside tern'));
    await tester.pumpAndSettle();
    expect(find.text('OLD TEXT of rev 1'), findsOneWidget);
    expect(find.text('Viewing revision 1'), findsOneWidget);
    await tester.tap(find.text('Back to latest'));
    await tester.pumpAndSettle();
    expect(find.text('NEW TEXT'), findsOneWidget);
  });

  test(
    'following the agent: a new document opens, an edit refreshes',
    () async {
      final api = _Api()..put(_meta('a', 'Report'), 'one');
      final docs = DocumentsState(api);
      await docs.refresh();
      await docs.touched(
        const DocumentTouch(docId: 'a', rev: 1, created: true, runId: 'r'),
        openNew: true,
      );
      expect(docs.open, ['a']);
      expect(docs.selected, 'a');

      // A second document, then an edit to the first while the second is up.
      api.put(_meta('b', 'Plan'), 'p');
      await docs.touched(
        const DocumentTouch(docId: 'b', rev: 1, created: true, runId: 'r'),
        openNew: true,
      );
      api
        ..texts['a'] = 'two'
        ..metas['a'] = _meta('a', 'Report', rev: 2);
      await docs.touched(
        const DocumentTouch(docId: 'a', rev: 2, created: false, runId: 'r'),
        openNew: false,
      );
      expect(docs.contentOf('a')!.content, 'two');
      expect(docs.unseen, {'a'}, reason: 'changed while out of sight');
      docs.select('a');
      expect(docs.unseen, isEmpty);

      // A background run's new document is listed, not opened.
      api.put(_meta('c', 'Routine output'), 'x');
      await docs.touched(
        const DocumentTouch(docId: 'c', rev: 1, created: true, runId: 'r2'),
        openNew: false,
      );
      expect(docs.open, isNot(contains('c')));
      expect(docs.all.map((d) => d.id), contains('c'));
    },
  );

  test('the tool output is parsed into a touch', () {
    final t = DocumentTouch.fromToolOutput(
      '{"doc_id":"d_1","rev":3,"changed":true}',
      created: false,
      runId: 'r',
    );
    expect(t!.docId, 'd_1');
    expect(t.rev, 3);
    expect(
      DocumentTouch.fromToolOutput('not json', created: true, runId: 'r'),
      isNull,
    );
    expect(
      DocumentTouch.fromToolOutput(
        '{"error":"stale"}',
        created: false,
        runId: 'r',
      ),
      isNull,
    );
  });

  test('csv parsing honours quotes and pads short rows', () {
    final rows = parseCsv('a,b,c\n1,"x, y",3\n"he said ""hi""",5\n');
    expect(rows[0], ['a', 'b', 'c']);
    expect(rows[1], ['1', 'x, y', '3']);
    expect(rows[2], ['he said "hi"', '5', '']);
  });
}
