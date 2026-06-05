# typed: true
# frozen_string_literal: true

# DOL — Docker Observability Language
#
# Install from a custom tap:
#   brew tap genc-murat/dockql https://github.com/genc-murat/DockQL
#   brew install dol
#
# Or directly from a local checkout:
#   brew install --formula _brew/dol.rb

class Dol < Formula
  desc "Docker Observability Language — a DSL for querying Docker infrastructure"
  homepage "https://github.com/genc-murat/DockQL"
  license "MIT"
  head "https://github.com/genc-murat/DockQL.git", branch: "main"

  stable do
    url "https://github.com/genc-murat/DockQL/archive/refs/tags/v0.6.0.tar.gz"
    # SHA256 will be updated after tag is pushed — run:
    #   curl -fsSL https://github.com/genc-murat/DockQL/archive/refs/tags/v0.6.0.tar.gz | shasum -a 256
    sha256 "UPDATE_AFTER_TAG_PUSH"
  end

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dol --version")
    assert_match "Docker Observability Language", shell_output("#{bin}/dol --help")
  end
end
