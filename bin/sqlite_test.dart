import 'dart:io';

import 'package:sqlite_async/sqlite_async.dart';
import 'package:path/path.dart' as path;

final migrations = SqliteMigrations()
  ..add(SqliteMigration(1, (tx) async {
    await tx.execute(
      'CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT)',
    );
  }));

// ignore: public_member_api_docs
void copyBinaryIfNecessary() {
  const libraryName = 'libsqlite3.so';
  final systemBinary = File('/lib/$libraryName')..createSync(recursive: true);

  final customBinary =
      File(path.join(Directory.current.path, 'target', 'debug', libraryName));
  customBinary.copySync(systemBinary.path);
}

void main() async {
  copyBinaryIfNecessary();
  final db = SqliteDatabase(path: 'simple-nubian');

  await migrations.migrate(db);

  // // Use execute() or executeBatch() for INSERT/UPDATE/DELETE statements
  // await db.executeBatch('INSERT INTO users(name, email) values(?, ?)', [
  //   ['Amen', 'oxy@gmail.com'],
  //   ['Moron', 'moron@gmail.com']
  // ]);

  // var results = await db.getAll('SELECT * FROM users');
  // print('Results: $results');

  // await db.writeTransaction((tx) async {
  //   await tx.execute(
  //     'INSERT INTO users(name, email) values(?, ?)',
  //     ['Test3', 'test3@example.com'],
  //   );
  //   await tx.execute(
  //     'INSERT INTO users(name, email) values(?, ?)',
  //     ['Test4', 'test4@example.com'],
  //   );
  // });

  // await db.close();
}
