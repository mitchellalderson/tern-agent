/// Prepares chat text for `MarkdownBody`: raw HTML tags are dropped (the
/// widget would print them), and a cut-off last paragraph gets its bold or
/// code span closed, so what the user sees is never literal markup.
String tidyMarkdown(String text) {
  final out = <String>[];
  var fenced = false;
  for (final line in text.split('\n')) {
    if (RegExp(r'^\s*(```|~~~)').hasMatch(line)) {
      fenced = !fenced;
      out.add(line);
    } else {
      out.add(fenced ? line : _stripTags(line));
    }
  }
  var s = out.join('\n');
  // A tag cut off at the very end: "</p", "<div cla".
  s = s.replaceFirst(RegExp(r'</?[a-zA-Z][^<>]*$|</?$'), '');
  return closeOpenSpans(s);
}

final _tag = RegExp(r'</?([a-zA-Z][a-zA-Z0-9]*)(\s[^<>]*)?/?>');
final _inlineCode = RegExp(r'`[^`]*`');

String _stripTags(String line) {
  // Inline code keeps its angle brackets.
  final buf = StringBuffer();
  var at = 0;
  for (final m in _inlineCode.allMatches(line)) {
    buf.write(_dropTags(line.substring(at, m.start)));
    buf.write(m.group(0));
    at = m.end;
  }
  buf.write(_dropTags(line.substring(at)));
  return buf.toString();
}

String _dropTags(String s) => s.replaceAllMapped(
  _tag,
  (m) => m.group(1)!.toLowerCase() == 'br' ? '\n' : '',
);

/// Closes a `**`, `` ` `` or code fence left open in the last paragraph; a
/// marker with nothing after it is removed instead.
String closeOpenSpans(String text) {
  var s = text.trimRight();
  final fences = RegExp(r'^\s*```', multiLine: true).allMatches(s).length;
  if (fences.isOdd) return '$s\n```';
  final start = s.lastIndexOf('\n\n') + 1;
  var para = s.substring(start);
  final ticks = '`'.allMatches(para).length;
  if (ticks.isOdd) {
    para = para.trimRight().endsWith('`')
        ? para.trimRight().substring(0, para.trimRight().length - 1)
        : '$para`';
  } else {
    final noCode = para.replaceAll(_inlineCode, '');
    if ('**'.allMatches(noCode).length.isOdd) {
      final i = para.lastIndexOf('**');
      para = para.substring(i + 2).trim().isEmpty
          ? para.substring(0, i)
          : '$para**';
    }
  }
  return (s.substring(0, start) + para).trimRight();
}
