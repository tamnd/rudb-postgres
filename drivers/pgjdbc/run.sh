# pgjdbc, which sends SELECT 1 through the extended flow. The jar comes from Maven Central.
set -e
version=42.7.8
jar=build/postgresql-$version.jar
java=${JAVA_HOME:+$JAVA_HOME/bin/}java
if [ ! -f "$jar" ]; then
  mkdir -p build
  curl -fsSL -o "$jar.part" "https://repo1.maven.org/maven2/org/postgresql/postgresql/$version/postgresql-$version.jar"
  mv "$jar.part" "$jar"
fi
"$java" -cp "$jar" Smoke.java
