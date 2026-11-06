import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/ui/collapse.dart';
import 'package:tern_desktop/ui/markdown_text.dart';

void main() {
  group('truncation inside bold', () {
    test('a cut-off bold span is closed', () {
      expect(
        tidyMarkdown('Inngest starts at **\$20 a mon'),
        'Inngest starts at **\$20 a mon**',
      );
    });

    test('a bold marker with nothing after it is dropped', () {
      expect(tidyMarkdown('Inngest starts at **'), 'Inngest starts at');
    });

    test('the collapsed preview closes a span it cut through', () {
      final lines = [
        for (var i = 0; i < 11; i++) 'line $i',
        '**Pricing starts at \$20',
        for (var i = 0; i < 20; i++) 'more $i',
      ];
      final p = collapsedPreview(lines.join('\n'))!;
      expect(p.text.endsWith('\$20**'), isTrue, reason: p.text);
    });

    test('a cut-off inline code span is closed', () {
      expect(tidyMarkdown('run `tern sta'), 'run `tern sta`');
    });
  });

  group('dollar amounts', () {
    test('text with \$ passes through unchanged', () {
      const s = 'From \$20 to \$300 a month, **\$5** per seat';
      expect(tidyMarkdown(s), s);
    });

    test('a bold marker followed by \$ and a half tag', () {
      expect(tidyMarkdown('Inngest starts at **\$ </p'), 'Inngest starts at **\$**');
    });
  });

  group('HTML-like text', () {
    test('tags are never shown', () {
      expect(
        tidyMarkdown('<p>Hello <b>world</b></p><br>next'),
        'Hello world\nnext',
      );
    });

    test('a tag cut off at the end is dropped', () {
      expect(tidyMarkdown('Hello world</'), 'Hello world');
      expect(tidyMarkdown('Hello <div cla'), 'Hello');
    });

    test('code keeps its angle brackets', () {
      const inline = 'use `<div>` here';
      expect(tidyMarkdown(inline), inline);
      const fenced = '```html\n<p>hi</p>\n```';
      expect(tidyMarkdown(fenced), fenced);
    });

    test('comparisons and autolinks are not tags', () {
      const s = 'if a < b and 3 > 2, see <https://inngest.com>';
      expect(tidyMarkdown(s), s);
    });
  });
}
