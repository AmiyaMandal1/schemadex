# Release process

## PyPI trusted publishing (PEP 740 / OIDC)

The release job in `.github/workflows/CI.yml` publishes to PyPI using OIDC
trusted publishing instead of a long-lived API token. The maintainer must
configure the trusted publisher on PyPI once before the first OIDC release.

### One-time PyPI configuration

Go to <https://pypi.org/manage/project/schemadex/settings/publishing/>, click
**Add trusted publisher**, select **GitHub**, and fill in:

- Owner: `AmiyaMandal1`
- Repository name: `schemadex`
- Workflow filename: `CI.yml`
- Environment name: (leave blank)

After that, the `PYPI_API_TOKEN` secret can be deleted.

### Cutover order

To avoid breaking the next release, follow these steps in order:

1. Configure the trusted publisher on PyPI **before** removing the token
   secret.
2. Push a `v0.1.x-rc1` tag — `release.yml` routes pre-release tags to TestPyPI,
   which is a safe dry-run of the OIDC flow.
3. Once the rc1 release is green, push the real `v0.1.x` tag.
4. Then delete the token secret:

   ```bash
   gh secret delete PYPI_API_TOKEN -R AmiyaMandal1/schemadex
   ```

The `id-token: write` permission required for OIDC is already set on the
`release` job in `CI.yml`.

## Documentation site (GitHub Pages)

`.github/workflows/docs.yml` builds the MkDocs Material site on every push to
`main` and deploys it to GitHub Pages.

Before the first deploy, the maintainer must enable Pages in the repo
settings:

- Go to <https://github.com/AmiyaMandal1/schemadex/settings/pages>.
- Under **Build and deployment**, set **Source** to **GitHub Actions**.

Once enabled, the next push to `main` will publish the site to
<https://amiyamandal1.github.io/schemadex/>.
