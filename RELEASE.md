## Release process

### Release branch
To release version `X.Y.Z`, first create a and checkout a branch named `release_vX.Y.Z`

```bash
export VF_VERSION=X.Y.Z
git checkout main
git fetch
git reset --hard origin/main
git switch -c release_$VF_VERSION
```

Then update all the package version strings to `X.Y.Z` using the `automation/bump_version.py` script

```bash
python automation/bump_version.py $VF_VERSION
```

Open a pull request to merge this branch into `main`.  This will start the continuous integration jobs on GitHub Actions. These jobs will test VegaFusion and build packages for publication.

### Publish Python packages
To publish the `vegafusion-python-embed` packages to PyPI, first download and unzip the `vegafusion-python-embed-wheels` artifacts from GitHub Actions. Then `cd` into the directory of `*.whl` and `*.tar.gz` files and upload the packages to PyPI using [twine](https://pypi.org/project/twine/).

```bash
cd vegafusion-python-embed-wheels
twine upload *
```

To publish the `vegafusion` packages, download and unzip the `vegafusion-wheel` artifacts. Then upload with twine.

```bash
cd vegafusion-wheel
twine upload *
```

To publish the `vegafusion-jupyter` packages, download and unzip the `vegafusion-jupyter-packages` artifacts. Then upload with twine.

```bash
cd vegafusion-jupyter-packages
twine upload *
```

### Publish NPM packages
First, download and unzip the `vegafusion-wasm-packages` artifact. Then publish the `vegafusion-wasm-X.Y.Z.tgz` package to NPM.  If this is a release candidate, include the `--pre` flag to `npm publish`.

```bash
cd vegafusion-wasm-packages
npm publish vegafusion-wasm-$VF_VERSION.tgz
```

Next, change the version of `vegafusion-wasm` in `javascript/vegafusion-embed/package.json` from `"../../vegafusion-wasm/pkg"` to `"~X.Y.Z"`

Then update `package.lock`, and build package, then publish to NPM (include the `--pre` flag to `npm publish` if this is a release candidate)
```bash
cd javascript/vegafusion-embed/
npm install
npm run build 
npm publish
```

Next, change the version of `vegafusion-wasm` and `vegafusion-embed` in `python/vegafusion-jupyter/package.json` from local paths to `"~X.Y.Z"`

Then build and publish the packages (include the `--pre` flag if this is a release candidate)

```bash
cd python/vegafusion-jupyter/
npm install
npm run build:prod
npm publish
```

### Publish Java library
First, download and unzip the `jni-native` CI artifact. This artifact contains the compiled JNI libraries for each supported operating system and architecture.

From the `java/` directory, set the `VEGAFUSION_JNI_LIBS` environment variable to the unzipped `jni-native` directory and publish the jar with `./gradlew publish`:

```
cd java/
VEGAFUSION_JNI_LIBS=/path/to/jni-native ./gradlew publish
```

This publishes the jar files to OSSRH at https://s01.oss.sonatype.org/. In order to sync these files to the public maven central repository, follow the steps described in https://central.sonatype.org/publish/release/.

#### Java publication config
Publishing the Java library to maven central requires configuring the `~/.gradle/gradle.properties` file with:
```
signing.keyId=YourKeyId
signing.password=YourPublicKeyPassword
signing.secretKeyRingFile=PathToYourKeyRingFile

ossrhUsername=your-jira-id
ossrhPassword=your-jira-password
```



### Publish Rust crates
The Rust crates should be published in the following order

```
cargo publish -p vegafusion-common
cargo publish -p vegafusion-core --no-verify
cargo publish -p vegafusion-dataframe
cargo publish -p vegafusion-datafusion-udfs
cargo publish -p vegafusion-sql
cargo publish -p vegafusion-runtime
cargo publish -p vegafusion-server
```

Note: the `--no-verify` flag for `vegafusion-core` is due to this cargo publish error:
```
Source directory was modified by build.rs during cargo publish. Build scripts should not modify anything outside of OUT_DIR.
```
We currently write the prost files to src (mostly to make it easier for IDEs to locate them). This should be safe in our case
as these aren't modified unless the .proto files change, but we should revisit where these files are written in the future.

