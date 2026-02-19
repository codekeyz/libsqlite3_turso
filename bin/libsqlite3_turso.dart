import 'dart:io';

import '../lib/database.dart';
import 'package:path/path.dart' as path;
import 'package:drift/drift.dart';

// ignore: public_member_api_docs
void copyBinaryIfNecessary() {
  const libraryName = 'libsqlite3.so';
  final systemBinary = File('/lib/$libraryName')..createSync(recursive: true);

  final customBinary = () {
    file({required bool debug}) => File(path.join(Directory.current.path,
        'target', debug ? 'debug' : 'release', libraryName));

    if (file(debug: false).existsSync()) {
      return file(debug: false);
    } else if (file(debug: true).existsSync()) {
      return file(debug: true);
    } else {
      throw Exception('No custom SQLite binary found in target directory.');
    }
  }();

  customBinary.copySync(systemBinary.path);
}

void main() async {
  copyBinaryIfNecessary();

  final database = AppDatabase();

  // await database.transaction(() async {
  //   // Create the table if it doesn't exist
  //   final result = await database.into(database.todoItems).insert(
  //         TodoItemsCompanion.insert(
  //           title: 'todo: setup drift',
  //           content: 'We need to set up drift for our SQLite database.',
  //         ),
  //       );

  // await (database
  //   .update(database.todoItems)
  //   ..where((tbl) => tbl.id.equals(result)))
  //   .write(TodoItemsCompanion(content: const Value('Updated content')));


  //   print('Inserted item with ID: $result');
  // });

  final allItems = await database.select(database.todoItems).get();
  for (final item in allItems) {
    print('Item: ${item.title}, Content: ${item.content}');
  }

  // final server = await HttpServer.bind(InternetAddress.anyIPv4, 8081);

  // var index = 0;

  // server.listen((request) async {
  //   final isEven = index % 2 == 0;

  //   final stopwatch = Stopwatch()..start();

  //   final result = await Future.wait([
  //     db.getAll('SELECT * FROM notes WHERE is_deleted = 0'),
  //     db.getAll(
  //       'SELECT * FROM notes WHERE id = ?',
  //       ['8703e588-c847-4cb6-b250-726db8afb49a'],
  //     ),
  //   ]);
  //   stopwatch.stop();

  //   print('Query took ${stopwatch.elapsedMilliseconds} ms');

  //   request.response
  //     ..statusCode = HttpStatus.ok
  //     ..headers.contentType = ContentType.json
  //     ..write(isEven ? result[0] : result[1])
  //     ..close();

  //   index++;
  // });

  // print('Server running on http://${server.address.address}:${server.port}');
  // ProcessSignal.sigterm.watch().listen((_) async {
  //   await server.close();
  //   await db.close();
  //   print('Server stopped.');
  // });
}
