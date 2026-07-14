defmodule ElixirGpuiWeb.Router do
  use ElixirGpuiWeb, :router

  pipeline :api do
    plug :accepts, ["json"]
  end

  scope "/api", ElixirGpuiWeb do
    pipe_through :api

    get "/status", DemoController, :status
  end

  scope "/", ElixirGpuiWeb do
    get "/", PageController, :index
  end
end
