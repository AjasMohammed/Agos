# Homebrew formula for AgentOS.
#
# Source of truth lives in-repo; publish to the tap "agentos/homebrew-tap":
#   brew tap agentos/tap && brew install agentos
#
# Downloads the prebuilt (signed) binary per arch — not a from-source build —
# so `brew install` is fast and matches the curl-installer trust path.
# On each release, Phase 09 bumps `version` + the four `sha256` values from the
# published `*.sha256` artifacts.
class Agentos < Formula
  desc "LLM-native operating system for AI agents"
  homepage "https://github.com/agentos/agentos"
  version "1.0.0"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/agentos/agentos/releases/download/v1.0.0/agentos-darwin-arm64"
      sha256 "REPLACE_WITH_DARWIN_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/agentos/agentos/releases/download/v1.0.0/agentos-darwin-amd64"
      sha256 "REPLACE_WITH_DARWIN_AMD64_SHA256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/agentos/agentos/releases/download/v1.0.0/agentos-linux-arm64"
      sha256 "REPLACE_WITH_LINUX_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/agentos/agentos/releases/download/v1.0.0/agentos-linux-amd64"
      sha256 "REPLACE_WITH_LINUX_AMD64_SHA256"
    end
  end

  def install
    bin.install Dir["agentos*"].first => "agentos"
  end

  def caveats
    <<~EOS
      Run `agentos onboard` to configure, then `agentos web serve`.

      On Linux, install bubblewrap for the shell-exec sandbox:
        sudo apt install bubblewrap
      On macOS, seccomp and most hardware (HAL) tools are unavailable and
      degrade gracefully — Linux is the primary target.
    EOS
  end

  test do
    assert_match "agentos", shell_output("#{bin}/agentos --version")
  end
end
