defmodule ElixirGpui.MixProject do
  use Mix.Project

  def project do
    [
      app: :elixir_gpui,
      version: "0.1.0",
      elixir: "~> 1.17",
      elixirc_paths: elixirc_paths(Mix.env()),
      start_permanent: Mix.env() == :prod,
      aliases: aliases(),
      deps: deps(),
      listeners: [Phoenix.CodeReloader]
    ]
  end

  # Configuration for the OTP application.
  #
  # Type `mix help compile.app` for more information.
  def application do
    [
      mod: {ElixirGpui.Application, []},
      extra_applications: [:logger, :runtime_tools]
    ]
  end

  def cli do
    [
      preferred_envs: [precommit: :test]
    ]
  end

  # Specifies which paths to compile per environment.
  defp elixirc_paths(:test), do: ["lib", "test/support"]
  defp elixirc_paths(_), do: ["lib"]

  # Specifies your project dependencies.
  #
  # Type `mix help deps` for examples and options.
  defp deps do
    [
      {:phoenix, "~> 1.8.9"},
      {:telemetry_metrics, "~> 1.0"},
      {:telemetry_poller, "~> 1.0"},
      {:jason, "~> 1.2"},
      {:dns_cluster, "~> 0.2.0"},
      {:bandit, "~> 1.5"},
      {:y_ex, "~> 0.10.5"}
    ]
  end

  # Aliases are shortcuts or tasks specific to the current project.
  # For example, to install project dependencies and perform other setup tasks, run:
  #
  #     $ mix setup
  #
  # See the documentation for `Mix` for more info on aliases.
  defp aliases do
    [
      setup: ["deps.get"],
      "ui.build": [&build_ui/1],
      "ui.serve": [&serve_ui/1],
      precommit: ["compile --warnings-as-errors", "deps.unlock --unused", "format", "test"]
    ]
  end

  defp build_ui(_), do: run_trunk(["build", "--release"])
  defp serve_ui(_), do: run_trunk(["serve"])

  defp run_trunk(arguments) do
    trunk =
      System.find_executable("trunk") ||
        Mix.raise("Trunk is unavailable. Install it with: cargo install trunk --locked")

    rustc =
      case System.cmd("rustup", ["which", "--toolchain", "nightly", "rustc"],
             stderr_to_stdout: true
           ) do
        {path, 0} -> String.trim(path)
        {output, _status} -> Mix.raise("Rust nightly is unavailable:\n#{output}")
      end

    path = Path.dirname(rustc) <> ":" <> System.get_env("PATH", "")

    case System.cmd(trunk, arguments,
           cd: "ui",
           env: [{"PATH", path}, {"NO_COLOR", "true"}],
           into: IO.stream(:stdio, :line),
           stderr_to_stdout: true
         ) do
      {_output, 0} -> :ok
      {_output, status} -> Mix.raise("Trunk failed with status #{status}")
    end
  end
end
