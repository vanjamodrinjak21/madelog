# Homebrew formula template for pop.
#
# This file is a template. The published formula lives in the tap repository
# `vanjamodrinjak/homebrew-pop` as `Formula/pop.rb`; nothing installs it from here.
#
# How the release workflow fills it in: after `.github/workflows/release.yml` has uploaded the
# ZIPs and `SHA256SUMS`, a tap job downloads `SHA256SUMS` from the release and rewrites the five
# placeholders below, one version and four checksums:
#
#   VERSION="${GITHUB_REF_NAME#v}"
#   sum() { grep " pop-${VERSION}-$1.zip\$" SHA256SUMS | cut -d' ' -f1; }
#   sed -e "s/__VERSION__/${VERSION}/g" \
#       -e "s/__SHA256_AARCH64_APPLE_DARWIN__/$(sum aarch64-apple-darwin)/" \
#       -e "s/__SHA256_X86_64_APPLE_DARWIN__/$(sum x86_64-apple-darwin)/" \
#       -e "s/__SHA256_AARCH64_LINUX__/$(sum aarch64-unknown-linux-gnu)/" \
#       -e "s/__SHA256_X86_64_LINUX__/$(sum x86_64-unknown-linux-gnu)/" \
#       packaging/homebrew/pop.rb > Formula/pop.rb
#
# The result is committed to the tap with a conventional commit, for example
# `chore: pop 1.2.3`. Keep the URL shape in sync with the archive names the release workflow
# produces: pop-<version>-<target>.zip.

class Pop < Formula
  desc "Record how your project came together, and prove it"
  homepage "https://pop.dev"
  version "__VERSION__"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/vanjamodrinjak/pop/releases/download/v#{version}/pop-#{version}-aarch64-apple-darwin.zip"
      sha256 "__SHA256_AARCH64_APPLE_DARWIN__"
    end

    on_intel do
      url "https://github.com/vanjamodrinjak/pop/releases/download/v#{version}/pop-#{version}-x86_64-apple-darwin.zip"
      sha256 "__SHA256_X86_64_APPLE_DARWIN__"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/vanjamodrinjak/pop/releases/download/v#{version}/pop-#{version}-aarch64-unknown-linux-gnu.zip"
      sha256 "__SHA256_AARCH64_LINUX__"
    end

    on_intel do
      url "https://github.com/vanjamodrinjak/pop/releases/download/v#{version}/pop-#{version}-x86_64-unknown-linux-gnu.zip"
      sha256 "__SHA256_X86_64_LINUX__"
    end
  end

  def install
    bin.install "pop"
    # The archives carry the licence next to the binary.
    prefix.install "LICENSE"
  end

  def caveats
    <<~EOS
      Run `pop onboard` once. It explains what pop records, checks for git, Git AI and Ollama,
      and offers to set up the repository you are in.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/pop --version")
    system bin/"pop", "--help"
  end
end
