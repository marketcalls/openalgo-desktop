import { Eye, EyeOff, Github, Info, Loader2, LogIn, MessageCircle } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { invoke } from '@tauri-apps/api/core'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useAuthStore } from '@/stores/authStore'

interface SetupCheckResponse {
  status: string
  needs_setup: boolean
}

interface LoginResponse {
  success: boolean
  user_id: number
  username: string
}

export default function Login() {
  const navigate = useNavigate()
  const { login: setLogin } = useAuthStore()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [showPassword, setShowPassword] = useState(false)
  const [isLoading, setIsLoading] = useState(false)
  const [isCheckingSetup, setIsCheckingSetup] = useState(true)
  const [error, setError] = useState<string | null>(null)

  // Check if setup is required or already logged in on page load
  useEffect(() => {
    const checkSetup = async () => {
      try {
        // First check if setup is needed (no users exist)
        const setupData = await invoke<SetupCheckResponse>('check_setup')
        if (setupData.needs_setup) {
          navigate('/setup', { replace: true })
          return
        }

        // Check if already logged in via Tauri command
        const isLoggedIn = await invoke<boolean>('check_session')
        if (isLoggedIn) {
          // Get user info to see if broker is connected
          const userInfo = await invoke<{ user_id: number; username: string } | null>(
            'get_current_user'
          )
          if (userInfo) {
            // Check broker status
            const brokerStatus = await invoke<{ connected: boolean; broker_id: string | null }>(
              'get_broker_status'
            )
            if (brokerStatus.connected && brokerStatus.broker_id) {
              // Fully logged in with broker, go to dashboard
              navigate('/dashboard', { replace: true })
              return
            } else {
              // Logged in but no broker, go to broker selection
              navigate('/broker', { replace: true })
              return
            }
          }
        }
        // Not logged in, stay on login page
      } catch (err) {
        console.error('Failed to check setup status:', err)
      } finally {
        setIsCheckingSetup(false)
      }
    }
    checkSetup()
  }, [navigate])

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setIsLoading(true)
    setError(null)

    try {
      // Use Tauri command for login
      const response = await invoke<LoginResponse>('login', {
        request: {
          username,
          password,
        },
      })

      if (response.success) {
        // Set login state (broker will be set after broker selection)
        setLogin(response.username, '')
        toast.success('Login successful')
        // Go to broker selection
        navigate('/broker')
      } else {
        setError('Login failed. Please try again.')
      }
    } catch (err: unknown) {
      console.error('Login error:', err)
      // Handle Tauri error format
      const errorMessage =
        err && typeof err === 'object' && 'message' in err
          ? (err as { message: string }).message
          : 'Invalid username or password'
      setError(errorMessage)
    } finally {
      setIsLoading(false)
    }
  }

  // Show loading while checking setup
  if (isCheckingSetup) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  return (
    <div className="min-h-screen flex items-center justify-center py-8 px-4">
      <div className="container max-w-6xl">
        <div className="flex flex-col lg:flex-row items-center justify-between gap-8 lg:gap-16">
          {/* Login Form - First on mobile */}
          <Card className="w-full max-w-md order-1 lg:order-2 shadow-xl">
            <CardHeader className="text-center">
              <div className="flex justify-center mb-4">
                <img src="/logo.png" alt="OpenAlgo" className="h-20 w-20" />
              </div>
              <CardTitle className="text-2xl">Welcome Back</CardTitle>
              <CardDescription>Sign in to your OpenAlgo account</CardDescription>
            </CardHeader>
            <CardContent>
              <form onSubmit={handleSubmit} className="space-y-4">
                <div className="space-y-2">
                  <Label htmlFor="username">Username</Label>
                  <Input
                    id="username"
                    type="text"
                    placeholder="Enter your username"
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    required
                    disabled={isLoading}
                    autoComplete="username"
                  />
                </div>

                <div className="space-y-2">
                  <Label htmlFor="password">Password</Label>
                  <div className="relative">
                    <Input
                      id="password"
                      type={showPassword ? 'text' : 'password'}
                      placeholder="Enter your password"
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                      required
                      disabled={isLoading}
                      autoComplete="current-password"
                      className="pr-10"
                    />
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="absolute right-0 top-0 h-full px-3 hover:bg-transparent"
                      onClick={() => setShowPassword(!showPassword)}
                      aria-label={showPassword ? 'Hide password' : 'Show password'}
                    >
                      {showPassword ? (
                        <EyeOff className="h-4 w-4 text-muted-foreground" />
                      ) : (
                        <Eye className="h-4 w-4 text-muted-foreground" />
                      )}
                    </Button>
                  </div>
                  <div className="text-right">
                    <Link
                      to="/reset-password"
                      className="text-sm text-muted-foreground hover:text-primary"
                    >
                      Forgot password?
                    </Link>
                  </div>
                </div>

                {error && (
                  <Alert variant="destructive">
                    <AlertDescription>{error}</AlertDescription>
                  </Alert>
                )}

                <Button type="submit" className="w-full" disabled={isLoading}>
                  {isLoading ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Signing in...
                    </>
                  ) : (
                    <>
                      <LogIn className="mr-2 h-4 w-4" />
                      Sign in
                    </>
                  )}
                </Button>
              </form>
            </CardContent>
          </Card>

          {/* Welcome Content - Second on mobile */}
          <div className="flex-1 max-w-xl text-center lg:text-left order-2 lg:order-1">
            <h1 className="text-4xl lg:text-5xl font-bold mb-6">
              Welcome to <span className="text-primary">OpenAlgo</span>
            </h1>
            <p className="text-lg lg:text-xl mb-8 text-muted-foreground">
              Sign in to your account to access your trading dashboard and manage your algorithmic
              trading strategies.
            </p>

            <Alert className="mb-6">
              <Info className="h-4 w-4" />
              <AlertTitle>First Time User?</AlertTitle>
              <AlertDescription>
                Contact your administrator to set up your account.
              </AlertDescription>
            </Alert>

            <div className="flex justify-center lg:justify-start gap-4">
              <Button variant="outline" asChild>
                <a
                  href="https://github.com/marketcalls/openalgo"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="flex items-center gap-2"
                >
                  <Github className="h-5 w-5" />
                  GitHub
                </a>
              </Button>
              <Button variant="outline" asChild>
                <a
                  href="https://openalgo.in/discord"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="flex items-center gap-2"
                >
                  <MessageCircle className="h-5 w-5" />
                  Discord
                </a>
              </Button>
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
