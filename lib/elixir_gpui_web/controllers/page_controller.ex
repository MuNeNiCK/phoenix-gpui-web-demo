defmodule ElixirGpuiWeb.PageController do
  use ElixirGpuiWeb, :controller

  def index(conn, _params) do
    path = Application.app_dir(:elixir_gpui, "priv/static/index.html")

    if File.regular?(path) do
      send_file(conn, 200, path)
    else
      conn
      |> put_resp_content_type("text/plain")
      |> send_resp(503, "GPUI Web has not been built. Run `mix assets.build` first.\n")
    end
  end
end
