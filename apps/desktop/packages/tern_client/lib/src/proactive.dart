import 'models.dart';

/// A standing "always" approval, as `tern grants` lists it.
class Grant {
  const Grant({required this.tool, required this.pattern, required this.note});

  factory Grant.fromJson(Json j) => Grant(
    tool: j['tool'] as String,
    pattern: j['pattern'] as String,
    note: j['note'] as String? ?? '',
  );

  final String tool;
  final String pattern;

  /// What it covers, in words.
  final String note;
}
