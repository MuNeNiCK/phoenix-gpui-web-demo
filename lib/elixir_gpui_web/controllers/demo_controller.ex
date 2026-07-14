defmodule ElixirGpuiWeb.DemoController do
  use ElixirGpuiWeb, :controller

  def status(conn, _params) do
    json(conn, %{
      service: "Phoenix",
      status: "online",
      elixir: System.version(),
      otp: System.otp_release(),
      gpui_web: "browser-wasm",
      checked_at: DateTime.utc_now() |> DateTime.truncate(:second) |> DateTime.to_iso8601()
    })
  end
end
