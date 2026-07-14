defmodule ElixirGpuiWeb.DemoControllerTest do
  use ElixirGpuiWeb.ConnCase, async: true

  test "GET /api/status reports the Elixir runtime", %{conn: conn} do
    conn = get(conn, "/api/status")

    assert %{
             "service" => "Phoenix",
             "status" => "online",
             "gpui_web" => "browser-wasm",
             "elixir" => elixir,
             "otp" => otp,
             "checked_at" => checked_at
           } = json_response(conn, 200)

    assert is_binary(elixir)
    assert is_binary(otp)
    assert {:ok, _, _} = DateTime.from_iso8601(checked_at)
  end
end
