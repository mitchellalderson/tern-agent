import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';

import '../../theme.dart';
import '../markdown_text.dart';
import '../widgets.dart';

/// Above this a document is shown as plain text: the Markdown widget builds
/// everything at once.
const maxRenderedBytes = 200 * 1024;

/// Renders a document's Markdown as native widgets. There is no script
/// engine to attack, and the rules are the document-safety rules
/// (docs/documents-design.md §5.2):
///
/// - raw HTML is dropped, never rendered;
/// - no image is fetched: a remote image is a placeholder naming its host;
/// - a link opens only after the full address is shown and confirmed, and
///   only `http`, `https` and `mailto` are links at all.
class SafeMarkdown extends StatelessWidget {
  const SafeMarkdown({super.key, required this.text, this.onOpenLink});

  final String text;

  /// Opens a confirmed link; the browser by default.
  final Future<void> Function(String url)? onOpenLink;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final theme = Theme.of(context);
    final body = theme.textTheme.bodyMedium!.copyWith(height: 1.55);
    if (text.length > maxRenderedBytes) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            'Large document, shown as plain text.',
            style: theme.textTheme.bodySmall,
          ),
          const SizedBox(height: 8),
          SelectableText(text, style: mono.copyWith(color: c.text)),
        ],
      );
    }
    return MarkdownBody(
      data: tidyMarkdown(text),
      selectable: true,
      onTapLink: (_, href, _) => _confirmLink(context, href),
      imageBuilder: (uri, title, alt) => _ImagePlaceholder(uri: uri, alt: alt),
      styleSheet: MarkdownStyleSheet.fromTheme(theme).copyWith(
        p: body,
        h1: body.copyWith(fontSize: 22, fontWeight: FontWeight.w600),
        h2: body.copyWith(fontSize: 18, fontWeight: FontWeight.w600),
        h3: body.copyWith(fontSize: 15, fontWeight: FontWeight.w600),
        code: mono.copyWith(color: c.text, backgroundColor: c.card),
        codeblockDecoration: BoxDecoration(
          color: c.card,
          border: Border.all(color: c.hairline),
          borderRadius: BorderRadius.circular(10),
        ),
        codeblockPadding: const EdgeInsets.all(12),
        blockquoteDecoration: BoxDecoration(
          border: Border(left: BorderSide(color: c.hairline, width: 3)),
        ),
        tableBorder: TableBorder.all(color: c.hairline),
        tableCellsPadding: const EdgeInsets.all(8),
        a: body.copyWith(
          color: c.link,
          fontWeight: FontWeight.w600,
          decoration: TextDecoration.underline,
        ),
        listBullet: body,
      ),
    );
  }

  Future<void> _confirmLink(BuildContext context, String? href) async {
    if (href == null) return;
    final uri = Uri.tryParse(href.trim());
    final scheme = uri?.scheme.toLowerCase();
    if (uri == null || !const {'http', 'https', 'mailto'}.contains(scheme)) {
      return; // Anything else is inert text.
    }
    final host = uri.host;
    final special = host.split('.').any((p) => p.startsWith('xn--'));
    final go = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Open this link?'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SelectableText(uri.toString(), style: mono),
            if (special) ...[
              const SizedBox(height: 10),
              const Text(
                'This address uses special characters and may not be '
                'the site it looks like.',
              ),
            ],
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          TextButton(
            onPressed: () =>
                Clipboard.setData(ClipboardData(text: uri.toString())),
            child: const Text('Copy link'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Open'),
          ),
        ],
      ),
    );
    if (go == true) await (onOpenLink ?? openUrl)(uri.toString());
  }
}

/// A document never fetches an image: it would tell the sender the document
/// was opened. Shows what it would have been.
class _ImagePlaceholder extends StatelessWidget {
  const _ImagePlaceholder({required this.uri, this.alt});

  final Uri uri;
  final String? alt;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final where = uri.hasAuthority ? uri.host : 'this document';
    final label = (alt?.trim().isNotEmpty ?? false) ? alt!.trim() : 'Image';
    return Container(
      margin: const EdgeInsets.symmetric(vertical: 4),
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
      decoration: BoxDecoration(
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.image_outlined, size: 16, color: c.muted),
          const SizedBox(width: 8),
          Flexible(
            child: Text(
              '$label · from $where, not loaded',
              style: TextStyle(color: c.muted, fontSize: 12),
            ),
          ),
        ],
      ),
    );
  }
}
